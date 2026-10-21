import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../background/background.dart';
import '../security/app_lock.dart';
import '../sigil/sigil_painter.dart';
import '../src/rust/api/call.dart';
import '../src/rust/api/sigil.dart';
import '../theme/tokens.dart';
import 'call_controller.dart';
import '../sigil/sigil_look.dart';

/// The colour a call screen takes from the pair's sigil.
Color callAccent(SigilSpec? sigil) => sigil?.look.accent.color ?? Palette.accents.first;

/// "4:12", or "1:02:03" past an hour.
String talkTime(Duration d) {
  final h = d.inHours, m = d.inMinutes % 60, s = d.inSeconds % 60;
  String two(int n) => n.toString().padLeft(2, '0');
  return h > 0 ? '$h:${two(m)}:${two(s)}' : '$m:${two(s)}';
}

/// What the call screen says under the name.
String callStatus(CallInfo c, String name, {Duration talked = Duration.zero}) => switch (c.phase) {
      CallPhaseView.connecting => 'Connecting privately…',
      CallPhaseView.ringing => c.outgoing ? 'Ringing…' : (c.video ? 'Incoming video call' : 'Incoming call'),
      CallPhaseView.active => c.nearby ? '${talkTime(talked)} · nearby' : talkTime(talked),
      CallPhaseView.ended => switch (c.outcome) {
          CallOutcomeView.missed => c.video ? 'Missed video call' : 'Missed call',
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

/// How the call travels, in plain words, for the top of the call screen.
({IconData icon, String label}) callPath(CallPathView? path) => switch (path) {
      CallPathView.direct => (icon: Icons.bolt_rounded, label: 'Direct'),
      CallPathView.nearby => (icon: Icons.wifi_rounded, label: 'Nearby'),
      CallPathView.private || null => (icon: Icons.lock_rounded, label: 'Private'),
    };

/// A line about the pictures, if there's something to say: the friend's
/// camera paused, or video kept private and so maybe choppy.
String? videoNote(CallMediaInfo? m, String name) {
  if (m == null) return null;
  if (m.peerCamera == CameraView.paused) return '$name\'s camera is paused';
  final video = m.camera != CameraView.off || m.peerCamera == CameraView.live;
  if (video && !m.sharp) return 'Video stays private, so it may be choppy';
  return null;
}

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
                      onAnswer: (video) => _calls.answer(video: video),
                      onEnd: _calls.hangUp,
                      onMinimize: c.phase == CallPhaseView.ended || locked ? null : () => setState(() => _small = true),
                    ),
            ),
          ),
      ],
    );
  }
}

/// The call, full screen: the pair's sigil (or the friend's picture), the
/// friend's name, how it's going, and mute / camera / end / speaker (or
/// answer / decline while ringing). Our own picture floats in a corner and
/// can be dragged.
class CallScreen extends StatefulWidget {
  const CallScreen({
    super.key,
    required this.info,
    required this.sigil,
    required this.locked,
    required this.onAnswer,
    required this.onEnd,
    this.onMinimize,
    this.remotePicture,
    this.localPicture,
  });

  final CallInfo info;
  final SigilSpec? sigil;

  /// The app is locked: don't say who it is.
  final bool locked;

  /// Answers: with the camera on (for a video call) or sound only.
  final Future<void> Function(bool video) onAnswer;
  final Future<void> Function() onEnd;
  final VoidCallback? onMinimize;

  /// Stand-ins for the pictures (tests have no camera or decoder).
  final Widget? remotePicture;
  final Widget? localPicture;

  @override
  State<CallScreen> createState() => _CallScreenState();
}

class _CallScreenState extends State<CallScreen> {
  final _calls = CallController.instance;
  Timer? _tick;

  /// Where our picture floats, from the top right.
  Offset _pip = Offset.zero;

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
    return ListenableBuilder(
      listenable: Listenable.merge([_calls.media, _calls.textures, _calls.camera, _calls.remoteFormat]),
      builder: (context, _) => _build(context),
    );
  }

  Widget _build(BuildContext context) {
    final c = widget.info;
    final anonymous = widget.locked || c.hidden;
    final name = anonymous ? 'Sanctum' : c.name;
    final accent = callAccent(anonymous ? null : widget.sigil);
    final answered = c.answeredAtMs;
    final talked = answered == null
        ? Duration.zero
        : DateTime.now().difference(DateTime.fromMillisecondsSinceEpoch(answered.toInt()));
    final ringing = c.phase == CallPhaseView.ringing && !c.outgoing;
    final ended = c.phase == CallPhaseView.ended;
    final active = c.phase == CallPhaseView.active;
    final m = _calls.media.value;
    final textures = _calls.textures.value;
    final remote = widget.remotePicture ??
        (textures == null ? null : _Picture(texture: textures.remote, format: _calls.remoteFormat));
    final local = widget.localPicture ??
        (textures == null ? null : _Picture(texture: textures.local, format: _calls.localFormat));
    // The friend's picture fills the screen while their camera is on.
    // A hidden chat's video waits for unlocking: it would show who it is.
    final seeing = active && m?.peerCamera == CameraView.live && remote != null && !(widget.locked && c.hidden);
    final showingMine = active && _calls.camera.value && local != null;
    final fg = seeing ? Colors.white : Palette.text;
    final path = callPath(active ? m?.path : null);
    final note = active ? videoNote(m, name) : null;
    return Material(
      color: seeing ? Colors.black : Palette.bg,
      child: Stack(
        fit: StackFit.expand,
        children: [
          if (seeing) ...[
            Positioned.fill(child: remote),
            // Keeps the words and buttons readable over any picture.
            const Positioned.fill(
              child: IgnorePointer(
                child: DecoratedBox(
                  decoration: BoxDecoration(
                    gradient: LinearGradient(
                      begin: Alignment.topCenter,
                      end: Alignment.bottomCenter,
                      colors: [Color(0x99000000), Color(0x00000000), Color(0x00000000), Color(0xB3000000)],
                      stops: [0, 0.22, 0.62, 1],
                    ),
                  ),
                ),
              ),
            ),
          ],
          SafeArea(
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
                            color: fg,
                            onTap: widget.onMinimize!,
                          ),
                        const Spacer(),
                        Icon(path.icon, size: 14, color: seeing ? Colors.white70 : Palette.textFaint),
                        const SizedBox(width: Space.xs),
                        Text(
                          '${path.label} ${c.video || seeing || showingMine ? 'video call' : 'call'}',
                          style: AppText.micro.copyWith(color: seeing ? Colors.white70 : null),
                        ),
                        const Spacer(),
                        if (widget.onMinimize != null) const SizedBox(width: 48),
                      ],
                    ),
                  ),
                  if (seeing) ...[
                    const SizedBox(height: Space.sm),
                    Text(
                      name,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: AppText.title.copyWith(color: Colors.white),
                    ),
                    const SizedBox(height: Space.xs),
                    Text(callStatus(c, name, talked: talked), style: AppText.caption.copyWith(color: Colors.white70)),
                    const Spacer(),
                  ] else ...[
                    const Spacer(),
                    _Emblem(sigil: anonymous ? null : widget.sigil, accent: accent, ended: ended),
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
                  ],
                  if (note != null) ...[
                    _Note(text: note, onVideo: seeing),
                    const SizedBox(height: Space.lg),
                  ],
                  if (ringing) _ringingButtons(c) else _talkingButtons(ended: ended, active: active, onVideo: seeing),
                  const SizedBox(height: Space.xl),
                ],
              ),
            ),
          ),
          if (showingMine) _floating(context, local),
        ],
      ),
    );
  }

  Widget _ringingButtons(CallInfo c) {
    return Column(
      mainAxisSize: MainAxisSize.min,
      children: [
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
              icon: c.video ? Icons.videocam_rounded : Icons.call_rounded,
              label: 'Answer',
              color: _answer,
              fg: Colors.white,
              onTap: () => widget.onAnswer(true),
            ),
          ],
        ),
        if (c.video) ...[
          const SizedBox(height: Space.md),
          TextButton.icon(
            onPressed: () => widget.onAnswer(false),
            icon: const Icon(Icons.call_rounded, size: 18),
            label: const Text('Answer without video'),
          ),
        ],
      ],
    );
  }

  Widget _talkingButtons({required bool ended, required bool active, required bool onVideo}) {
    final m = _calls.media.value;
    final canVideo = active && (m?.canSendVideo ?? widget.info.sendVideoCodec.isNotEmpty);
    return AnimatedOpacity(
      opacity: ended ? 0.35 : 1,
      duration: Motion.medium,
      child: IgnorePointer(
        ignoring: ended,
        child: Row(
          mainAxisAlignment: MainAxisAlignment.spaceEvenly,
          children: [
            ValueListenableBuilder<bool>(
              valueListenable: _calls.muted,
              builder: (context, muted, _) => _RoundButton(
                icon: muted ? Icons.mic_off_rounded : Icons.mic_rounded,
                label: muted ? 'Unmute' : 'Mute',
                on: muted,
                onVideo: onVideo,
                onTap: _calls.toggleMute,
              ),
            ),
            if (canVideo)
              _RoundButton(
                icon: _calls.camera.value ? Icons.videocam_rounded : Icons.videocam_off_rounded,
                label: _calls.camera.value ? 'Camera on' : 'Camera off',
                on: _calls.camera.value,
                onVideo: onVideo,
                onTap: _calls.toggleCamera,
              ),
            _RoundButton(
              icon: Icons.call_end_rounded,
              label: 'End',
              color: Palette.danger,
              fg: Colors.white,
              onVideo: onVideo,
              onTap: widget.onEnd,
            ),
            ValueListenableBuilder<bool>(
              valueListenable: _calls.speaker,
              builder: (context, speaker, _) => _RoundButton(
                icon: speaker ? Icons.volume_up_rounded : Icons.hearing_rounded,
                label: speaker ? 'Speaker' : 'Earpiece',
                on: speaker,
                onVideo: onVideo,
                onTap: _calls.toggleSpeaker,
              ),
            ),
          ],
        ),
      ),
    );
  }

  /// Our own picture, small, in a corner; drag it anywhere.
  Widget _floating(BuildContext context, Widget picture) {
    const width = 108.0, height = 148.0;
    final size = MediaQuery.sizeOf(context);
    final pad = MediaQuery.paddingOf(context);
    final maxX = (size.width - width - 2 * Space.lg).clamp(0.0, double.infinity);
    final maxY = (size.height - height - pad.top - pad.bottom - 2 * Space.lg).clamp(0.0, double.infinity);
    final dx = _pip.dx.clamp(0.0, maxX), dy = _pip.dy.clamp(0.0, maxY);
    return Positioned(
      key: const ValueKey('my-camera'),
      right: Space.lg + dx,
      top: pad.top + Space.lg + 56 + dy,
      width: width,
      height: height,
      child: GestureDetector(
        onPanUpdate: (d) => setState(() => _pip = Offset(dx - d.delta.dx, dy + d.delta.dy)),
        child: Semantics(
          container: true,
          label: 'Your camera',
          child: DecoratedBox(
            decoration: BoxDecoration(
              borderRadius: BorderRadius.circular(Radii.lg),
              border: Border.all(color: Colors.white24),
            ),
            child: ClipRRect(
              borderRadius: BorderRadius.circular(Radii.lg),
              child: Stack(
                fit: StackFit.expand,
                children: [
                  ColoredBox(color: Colors.black, child: picture),
                  Positioned(
                    right: Space.xs,
                    bottom: Space.xs,
                    child: ValueListenableBuilder<bool>(
                      valueListenable: _calls.frontCamera,
                      builder: (context, front, _) => Material(
                        color: const Color(0x66000000),
                        shape: const CircleBorder(),
                        clipBehavior: Clip.antiAlias,
                        child: InkWell(
                          onTap: () {
                            HapticFeedback.selectionClick();
                            _calls.flipCamera();
                          },
                          child: Semantics(
                            button: true,
                            label: front ? 'Use the back camera' : 'Use the front camera',
                            excludeSemantics: true,
                            child: const SizedBox.square(
                              dimension: 36,
                              child: Icon(Icons.flip_camera_android_rounded, size: 20, color: Colors.white),
                            ),
                          ),
                        ),
                      ),
                    ),
                  ),
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }

  static const _answer = Color(0xFF3DDC97);
}

/// A picture from the Android side: a Flutter texture, turned and
/// mirrored as it says, filling its box.
class _Picture extends StatelessWidget {
  const _Picture({required this.texture, required this.format});

  final int texture;
  final ValueListenable<VideoFormat?> format;

  @override
  Widget build(BuildContext context) => ValueListenableBuilder<VideoFormat?>(
        valueListenable: format,
        builder: (context, f, _) {
          if (f == null || f.width <= 0 || f.height <= 0) return const ColoredBox(color: Colors.black);
          Widget picture = SizedBox(
            width: f.width.toDouble(),
            height: f.height.toDouble(),
            child: Texture(textureId: texture),
          );
          picture = RotatedBox(quarterTurns: f.rotation ~/ 90, child: picture);
          if (f.mirror) picture = Transform.flip(flipX: true, child: picture);
          return ClipRect(child: FittedBox(fit: BoxFit.cover, child: picture));
        },
      );
}

/// The pair's sigil in a ring (or a plain call icon when it mustn't say
/// who).
class _Emblem extends StatelessWidget {
  const _Emblem({required this.sigil, required this.accent, required this.ended});

  final SigilSpec? sigil;
  final Color accent;
  final bool ended;

  @override
  Widget build(BuildContext context) => SizedBox.square(
        dimension: 220,
        child: DecoratedBox(
          decoration: BoxDecoration(
            shape: BoxShape.circle,
            border: Border.all(color: accent.withValues(alpha: ended ? 0.15 : 0.45), width: 2),
          ),
          child: Padding(
            padding: const EdgeInsets.all(Space.sm),
            child: ClipOval(
              child: sigil == null
                  ? ColoredBox(
                      color: Palette.surfaceHigh,
                      child: Icon(Icons.call_rounded, size: 72, color: Palette.textMuted),
                    )
                  : LivingSigil(spec: sigil!, period: 40),
            ),
          ),
        ),
      );
}

/// A short line about the pictures, in a soft pill.
class _Note extends StatelessWidget {
  const _Note({required this.text, required this.onVideo});

  final String text;
  final bool onVideo;

  @override
  Widget build(BuildContext context) => Container(
        padding: const EdgeInsets.symmetric(horizontal: Space.md, vertical: Space.xs + 2),
        decoration: BoxDecoration(
          color: onVideo ? const Color(0x66000000) : Palette.surfaceHigh,
          borderRadius: BorderRadius.circular(Radii.lg),
        ),
        child: Text(
          text,
          textAlign: TextAlign.center,
          style: AppText.caption.copyWith(color: onVideo ? Colors.white : Palette.textMuted),
        ),
      );
}

class _RoundButton extends StatelessWidget {
  const _RoundButton({
    required this.icon,
    required this.label,
    required this.onTap,
    this.color,
    this.fg,
    this.on = false,
    this.onVideo = false,
  });

  final IconData icon;
  final String label;

  /// Fixed colours (end, answer); otherwise a toggle, filled when `on`.
  final Color? color;
  final Color? fg;
  final bool on;

  /// Over the friend's picture: lighter colours.
  final bool onVideo;
  final Future<void> Function() onTap;

  @override
  Widget build(BuildContext context) {
    final background =
        color ?? (onVideo ? (on ? Colors.white : const Color(0x40FFFFFF)) : (on ? Palette.text : Palette.surfaceHigh));
    final foreground = fg ?? (onVideo ? (on ? Colors.black : Colors.white) : (on ? Palette.bg : Palette.text));
    return Semantics(
      button: true,
      label: label,
      excludeSemantics: true,
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          Material(
            color: background,
            shape: CircleBorder(side: BorderSide(color: onVideo ? Colors.white24 : Palette.outline)),
            clipBehavior: Clip.antiAlias,
            child: InkWell(
              onTap: () {
                HapticFeedback.selectionClick();
                onTap();
              },
              child: SizedBox.square(dimension: 64, child: Icon(icon, size: 28, color: foreground)),
            ),
          ),
          const SizedBox(height: Space.sm),
          Text(label, style: AppText.caption.copyWith(color: onVideo ? Colors.white : null)),
        ],
      ),
    );
  }
}

class _IconTap extends StatelessWidget {
  const _IconTap({required this.icon, required this.label, required this.color, required this.onTap});

  final IconData icon;
  final String label;
  final Color color;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) => Semantics(
        button: true,
        label: label,
        excludeSemantics: true,
        child: InkResponse(
          onTap: onTap,
          radius: 24,
          child: SizedBox.square(dimension: 48, child: Icon(icon, size: 28, color: color)),
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
                    Icon(c.video ? Icons.videocam_rounded : Icons.call_rounded, size: 16, color: fg),
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
