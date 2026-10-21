import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../security/app_lock.dart';
import '../sigil/sigil_painter.dart';
import '../src/rust/api/call.dart';
import '../src/rust/api/sigil.dart';
import '../theme/tokens.dart';
import 'call_controller.dart';

/// The colour a call screen takes from the pair's sigil.
Color callAccent(SigilSpec? sigil) => sigil?.accent.color ?? Palette.accents.first;

/// "4:12", or "1:02:03" past an hour.
String talkTime(Duration d) {
  final h = d.inHours, m = d.inMinutes % 60, s = d.inSeconds % 60;
  String two(int n) => n.toString().padLeft(2, '0');
  return h > 0 ? '$h:${two(m)}:${two(s)}' : '$m:${two(s)}';
}

/// What the call screen says under the name.
String callStatus(CallInfo c, String name, {Duration talked = Duration.zero}) => switch (c.phase) {
      CallPhaseView.connecting => 'Connecting privately…',
      CallPhaseView.ringing => c.outgoing ? 'Ringing…' : 'Incoming call',
      CallPhaseView.active => c.nearby ? '${talkTime(talked)} · nearby' : talkTime(talked),
      CallPhaseView.ended => switch (c.outcome) {
          CallOutcomeView.missed => 'Missed call',
          CallOutcomeView.declined => c.outgoing ? '$name can\'t talk right now' : 'Declined',
          CallOutcomeView.noAnswer => 'No answer',
          CallOutcomeView.unreachable => '$name isn\'t reachable right now',
          CallOutcomeView.busy => '$name is on another call',
          CallOutcomeView.failed => 'The call couldn\'t connect',
          CallOutcomeView.dropped => 'The call dropped',
          CallOutcomeView.cancelled => 'Call cancelled',
          CallOutcomeView.talked || null => 'Call ended',
        },
    };

/// Shows the call above everything, the app lock included, so a call can
/// be answered and ended without unlocking. While locked, or for a hidden
/// chat, it doesn't say who's calling. Can be shrunk to a bar to use the
/// app during a call.
class CallLayer extends StatefulWidget {
  const CallLayer({super.key, required this.child});

  final Widget child;

  @override
  State<CallLayer> createState() => _CallLayerState();
}

class _CallLayerState extends State<CallLayer> {
  final _calls = CallController.instance;
  bool _small = false;
  String? _sigilFor;
  Future<SigilSpec>? _sigil;

  @override
  void initState() {
    super.initState();
    _calls.call.addListener(_changed);
  }

  @override
  void dispose() {
    _calls.call.removeListener(_changed);
    super.dispose();
  }

  void _changed() {
    final c = _calls.call.value;
    // A new call, or one ringing: full screen.
    if (c == null || c.id != _sigilFor || c.phase == CallPhaseView.ended) _small = false;
    if (c != null && c.id != _sigilFor) {
      _sigilFor = c.id;
      _sigil = sigilForKey(publicKey: c.sigilKey);
    }
    setState(() {});
  }

  @override
  Widget build(BuildContext context) {
    final c = _calls.call.value;
    return Stack(
      fit: StackFit.expand,
      children: [
        widget.child,
        if (c != null)
          FutureBuilder<SigilSpec>(
            future: _sigil,
            builder: (context, snap) => ValueListenableBuilder<bool>(
              valueListenable: AppLock.locked,
              builder: (context, locked, _) => _small
                  ? _CallBar(info: c, accent: callAccent(snap.data), onTap: () => setState(() => _small = false))
                  : CallScreen(
                      info: c,
                      sigil: snap.data,
                      locked: locked,
                      onAnswer: _calls.answer,
                      onEnd: _calls.hangUp,
                      onMinimize: c.phase == CallPhaseView.ended || locked ? null : () => setState(() => _small = true),
                    ),
            ),
          ),
      ],
    );
  }
}

/// The call, full screen: the pair's sigil, the friend's name, how it's
/// going, and mute / speaker / end (or answer / decline while ringing).
class CallScreen extends StatefulWidget {
  const CallScreen({
    super.key,
    required this.info,
    required this.sigil,
    required this.locked,
    required this.onAnswer,
    required this.onEnd,
    this.onMinimize,
  });

  final CallInfo info;
  final SigilSpec? sigil;

  /// The app is locked: don't say who it is.
  final bool locked;
  final Future<void> Function() onAnswer;
  final Future<void> Function() onEnd;
  final VoidCallback? onMinimize;

  @override
  State<CallScreen> createState() => _CallScreenState();
}

class _CallScreenState extends State<CallScreen> {
  Timer? _tick;

  @override
  void initState() {
    super.initState();
    _tick = Timer.periodic(const Duration(seconds: 1), (_) {
      if (widget.info.phase == CallPhaseView.active) setState(() {});
    });
  }

  @override
  void dispose() {
    _tick?.cancel();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final c = widget.info;
    final calls = CallController.instance;
    final anonymous = widget.locked || c.hidden;
    final name = anonymous ? 'Sanctum' : c.name;
    final accent = callAccent(anonymous ? null : widget.sigil);
    final answered = c.answeredAtMs;
    final talked = answered == null
        ? Duration.zero
        : DateTime.now().difference(DateTime.fromMillisecondsSinceEpoch(answered.toInt()));
    final ringing = c.phase == CallPhaseView.ringing && !c.outgoing;
    final ended = c.phase == CallPhaseView.ended;
    return Material(
      color: Palette.bg,
      child: SafeArea(
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: Space.xl, vertical: Space.lg),
          child: Column(
            children: [
              SizedBox(
                height: 48,
                child: Row(
                  children: [
                    if (widget.onMinimize != null)
                      _IconTap(
                        icon: Icons.keyboard_arrow_down_rounded,
                        label: 'Back to chats',
                        onTap: widget.onMinimize!,
                      ),
                    const Spacer(),
                    Icon(Icons.lock_rounded, size: 14, color: Palette.textFaint),
                    const SizedBox(width: Space.xs),
                    Text('Private call', style: AppText.micro),
                    const Spacer(),
                    if (widget.onMinimize != null) const SizedBox(width: 48),
                  ],
                ),
              ),
              const Spacer(),
              SizedBox.square(
                dimension: 220,
                child: DecoratedBox(
                  decoration: BoxDecoration(
                    shape: BoxShape.circle,
                    border: Border.all(color: accent.withValues(alpha: ended ? 0.15 : 0.45), width: 2),
                  ),
                  child: Padding(
                    padding: const EdgeInsets.all(Space.sm),
                    child: ClipOval(
                      child: widget.sigil == null || anonymous
                          ? ColoredBox(
                              color: Palette.surfaceHigh,
                              child: Icon(Icons.call_rounded, size: 72, color: Palette.textMuted),
                            )
                          : LivingSigil(spec: widget.sigil!, period: 40),
                    ),
                  ),
                ),
              ),
              const SizedBox(height: Space.xl),
              Text(
                name,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                textAlign: TextAlign.center,
                style: AppText.display.copyWith(color: Palette.text),
              ),
              const SizedBox(height: Space.sm),
              AnimatedSwitcher(
                duration: Motion.fast,
                child: Text(
                  callStatus(c, name, talked: talked),
                  key: ValueKey(c.phase),
                  textAlign: TextAlign.center,
                  style: AppText.body.copyWith(color: ended ? Palette.textMuted : accent),
                ),
              ),
              const Spacer(flex: 2),
              if (ringing)
                Row(
                  mainAxisAlignment: MainAxisAlignment.spaceEvenly,
                  children: [
                    _RoundButton(
                      icon: Icons.call_end_rounded,
                      label: 'Decline',
                      color: Palette.danger,
                      fg: Colors.white,
                      onTap: widget.onEnd,
                    ),
                    _RoundButton(
                      icon: Icons.call_rounded,
                      label: 'Answer',
                      color: _answer,
                      fg: Colors.white,
                      onTap: widget.onAnswer,
                    ),
                  ],
                )
              else
                AnimatedOpacity(
                  opacity: ended ? 0.35 : 1,
                  duration: Motion.medium,
                  child: IgnorePointer(
                    ignoring: ended,
                    child: Row(
                      mainAxisAlignment: MainAxisAlignment.spaceEvenly,
                      children: [
                        ValueListenableBuilder<bool>(
                          valueListenable: calls.muted,
                          builder: (context, muted, _) => _RoundButton(
                            icon: muted ? Icons.mic_off_rounded : Icons.mic_rounded,
                            label: muted ? 'Unmute' : 'Mute',
                            color: muted ? Palette.text : Palette.surfaceHigh,
                            fg: muted ? Palette.bg : Palette.text,
                            onTap: calls.toggleMute,
                          ),
                        ),
                        _RoundButton(
                          icon: Icons.call_end_rounded,
                          label: 'End',
                          color: Palette.danger,
                          fg: Colors.white,
                          onTap: widget.onEnd,
                        ),
                        ValueListenableBuilder<bool>(
                          valueListenable: calls.speaker,
                          builder: (context, speaker, _) => _RoundButton(
                            icon: speaker ? Icons.volume_up_rounded : Icons.hearing_rounded,
                            label: speaker ? 'Speaker' : 'Earpiece',
                            color: speaker ? Palette.text : Palette.surfaceHigh,
                            fg: speaker ? Palette.bg : Palette.text,
                            onTap: calls.toggleSpeaker,
                          ),
                        ),
                      ],
                    ),
                  ),
                ),
              const SizedBox(height: Space.xl),
            ],
          ),
        ),
      ),
    );
  }

  static const _answer = Color(0xFF3DDC97);
}

class _RoundButton extends StatelessWidget {
  const _RoundButton({
    required this.icon,
    required this.label,
    required this.color,
    required this.fg,
    required this.onTap,
  });

  final IconData icon;
  final String label;
  final Color color;
  final Color fg;
  final Future<void> Function() onTap;

  @override
  Widget build(BuildContext context) {
    return Semantics(
      button: true,
      label: label,
      excludeSemantics: true,
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          Material(
            color: color,
            shape: CircleBorder(side: BorderSide(color: Palette.outline)),
            clipBehavior: Clip.antiAlias,
            child: InkWell(
              onTap: () {
                HapticFeedback.selectionClick();
                onTap();
              },
              child: SizedBox.square(dimension: 72, child: Icon(icon, size: 30, color: fg)),
            ),
          ),
          const SizedBox(height: Space.sm),
          Text(label, style: AppText.caption),
        ],
      ),
    );
  }
}

class _IconTap extends StatelessWidget {
  const _IconTap({required this.icon, required this.label, required this.onTap});

  final IconData icon;
  final String label;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) => Semantics(
        button: true,
        label: label,
        excludeSemantics: true,
        child: InkResponse(
          onTap: onTap,
          radius: 24,
          child: SizedBox.square(dimension: 48, child: Icon(icon, size: 28, color: Palette.text)),
        ),
      );
}

/// The call, shrunk to a bar at the top while using the app.
class _CallBar extends StatefulWidget {
  const _CallBar({required this.info, required this.accent, required this.onTap});

  final CallInfo info;
  final Color accent;
  final VoidCallback onTap;

  @override
  State<_CallBar> createState() => _CallBarState();
}

class _CallBarState extends State<_CallBar> {
  late final Timer _tick = Timer.periodic(const Duration(seconds: 1), (_) => setState(() {}));

  @override
  void dispose() {
    _tick.cancel();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final c = widget.info;
    final name = c.hidden ? 'Sanctum' : c.name;
    final answered = c.answeredAtMs;
    final talked = answered == null
        ? Duration.zero
        : DateTime.now().difference(DateTime.fromMillisecondsSinceEpoch(answered.toInt()));
    final fg = Palette.onColor(widget.accent);
    return Align(
      alignment: Alignment.topCenter,
      child: SafeArea(
        child: Padding(
          padding: const EdgeInsets.all(Space.sm),
          child: Material(
            color: widget.accent,
            borderRadius: BorderRadius.circular(Radii.lg),
            clipBehavior: Clip.antiAlias,
            child: InkWell(
              onTap: widget.onTap,
              child: Padding(
                padding: const EdgeInsets.symmetric(horizontal: Space.lg, vertical: Space.sm),
                child: Row(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    Icon(Icons.call_rounded, size: 16, color: fg),
                    const SizedBox(width: Space.sm),
                    Flexible(
                      child: Text(
                        '$name · ${callStatus(c, name, talked: talked)}',
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: AppText.bodyStrong.copyWith(color: fg),
                      ),
                    ),
                  ],
                ),
              ),
            ),
          ),
        ),
      ),
    );
  }
}
