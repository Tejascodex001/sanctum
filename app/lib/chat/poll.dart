import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart' show Int64List;

import '../src/rust/api/poll.dart';
import '../theme/tokens.dart';

const _weekdays = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'];
const _months = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];

DateTime _fromUnix(int s) => DateTime.fromMillisecondsSinceEpoch(s * 1000);
int _toUnix(DateTime t) => t.millisecondsSinceEpoch ~/ 1000;

/// "Sat 25 Oct, 19:30" (the year only when it isn't this year; no time
/// when it's midnight, i.e. a whole day).
String formatWhen(DateTime t) {
  final now = DateTime.now();
  final day = '${_weekdays[t.weekday - 1]} ${t.day} ${_months[t.month - 1]}${t.year == now.year ? '' : ' ${t.year}'}';
  if (t.hour == 0 && t.minute == 0) return day;
  return '$day, ${t.hour.toString().padLeft(2, '0')}:${t.minute.toString().padLeft(2, '0')}';
}

/// What the chat list shows for a poll.
String pollPreview(PollView p) => p.plan != null ? 'Plan: ${p.question}' : 'Poll: ${p.question}';

/// A poll, date poll or plan inside a message bubble. Tapping a choice
/// answers (tap again to take it back).
class PollContent extends StatelessWidget {
  const PollContent({
    super.key,
    required this.poll,
    required this.chatId,
    required this.seq,
    required this.fg,
    required this.accent,
    required this.mine,
    this.onChanged,
  });

  final PollView poll;
  final int chatId;
  final int seq;
  final Color fg;
  final Color accent;

  /// Our own message (drawn on the accent colour).
  final bool mine;
  final VoidCallback? onChanged;

  Future<void> _tap(int i) async {
    HapticFeedback.selectionClick();
    final chosen = [
      for (final (j, o) in poll.options.indexed)
        if (o.mine) j,
    ];
    final List<int> next;
    if (chosen.contains(i)) {
      next = chosen..remove(i);
    } else if (poll.multi) {
      next = chosen..add(i);
    } else {
      next = [i];
    }
    await pollVote(chatId: chatId, seq: seq, choices: next);
    onChanged?.call();
  }

  @override
  Widget build(BuildContext context) {
    final plan = poll.plan;
    final faint = fg.withValues(alpha: 0.65);
    return ConstrainedBox(
      constraints: const BoxConstraints(minWidth: 220),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        mainAxisSize: MainAxisSize.min,
        children: [
          Row(
            children: [
              Icon(plan != null ? Icons.event_rounded : Icons.poll_rounded, size: 16, color: faint),
              const SizedBox(width: Space.xs),
              Text(
                plan != null ? 'PLAN' : (poll.dates ? 'WHEN WORKS?' : 'POLL'),
                style: AppText.overline.copyWith(color: faint, letterSpacing: 1.4),
              ),
            ],
          ),
          const SizedBox(height: Space.xs),
          Text(poll.question, style: AppText.heading.copyWith(color: fg)),
          if (plan != null) ...[
            const SizedBox(height: Space.xs),
            _Detail(icon: Icons.schedule_rounded, text: formatWhen(_fromUnix(plan.when)), color: faint),
            if (plan.place.isNotEmpty) _Detail(icon: Icons.place_outlined, text: plan.place, color: faint),
            const SizedBox(height: Space.sm),
            Row(
              children: [
                for (final (i, o) in poll.options.indexed) ...[
                  if (i > 0) const SizedBox(width: Space.xs),
                  Expanded(child: _PlanChoice(option: o, fg: fg, accent: accent, mine: mine, onTap: () => _tap(i))),
                ],
              ],
            ),
            for (final o in poll.options)
              if (o.voters.isNotEmpty)
                Padding(
                  padding: const EdgeInsets.only(top: Space.xs),
                  child: Text('${o.label}: ${o.voters.join(', ')}', style: AppText.micro.copyWith(color: faint)),
                ),
          ] else ...[
            if (poll.multi)
              Padding(
                padding: const EdgeInsets.only(top: 2),
                child: Text('Choose any', style: AppText.micro.copyWith(color: faint)),
              ),
            const SizedBox(height: Space.sm),
            for (final (i, o) in poll.options.indexed)
              _Choice(
                label: o.date == null
                    ? o.label
                    : (o.label.isEmpty
                        ? formatWhen(_fromUnix(o.date!))
                        : '${formatWhen(_fromUnix(o.date!))} · ${o.label}'),
                option: o,
                total: poll.answered,
                fg: fg,
                accent: accent,
                mine: mine,
                onTap: () => _tap(i),
              ),
            const SizedBox(height: 2),
            Text(
              switch (poll.answered) {
                0 => 'No answers yet',
                1 => '1 answer',
                final n => '$n answers',
              },
              style: AppText.micro.copyWith(color: faint),
            ),
          ],
        ],
      ),
    );
  }
}

class _Detail extends StatelessWidget {
  const _Detail({required this.icon, required this.text, required this.color});

  final IconData icon;
  final String text;
  final Color color;

  @override
  Widget build(BuildContext context) => Padding(
        padding: const EdgeInsets.only(top: 2),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            Icon(icon, size: 14, color: color),
            const SizedBox(width: Space.xs),
            Flexible(child: Text(text, style: AppText.caption.copyWith(color: color))),
          ],
        ),
      );
}

/// One poll choice: a bar filled by its share of the answers.
class _Choice extends StatelessWidget {
  const _Choice({
    required this.label,
    required this.option,
    required this.total,
    required this.fg,
    required this.accent,
    required this.mine,
    required this.onTap,
  });

  final String label;
  final PollOptionView option;
  final int total;
  final Color fg;
  final Color accent;
  final bool mine;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final share = total == 0 ? 0.0 : option.count / total;
    // On our own (accent) bubble the fill is the text colour, faintly.
    final fill = mine ? fg.withValues(alpha: 0.18) : accent.withValues(alpha: 0.28);
    final border = option.mine ? (mine ? fg : accent) : fg.withValues(alpha: 0.18);
    return Padding(
      padding: const EdgeInsets.only(bottom: Space.xs),
      child: Material(
        type: MaterialType.transparency,
        child: InkWell(
          onTap: onTap,
          borderRadius: BorderRadius.circular(Radii.sm),
          child: Container(
            decoration: BoxDecoration(
              borderRadius: BorderRadius.circular(Radii.sm),
              border: Border.all(color: border, width: option.mine ? 1.6 : 1),
            ),
            clipBehavior: Clip.antiAlias,
            child: Stack(
              children: [
                Positioned.fill(
                  child: Align(
                    alignment: Alignment.centerLeft,
                    child: AnimatedFractionallySizedBox(
                      duration: Motion.medium,
                      curve: Motion.emphasized,
                      widthFactor: share,
                      heightFactor: 1,
                      child: ColoredBox(color: fill),
                    ),
                  ),
                ),
                Padding(
                  padding: const EdgeInsets.symmetric(horizontal: Space.md, vertical: 9),
                  child: Row(
                    children: [
                      if (option.mine) ...[
                        Icon(Icons.check_circle_rounded, size: 16, color: mine ? fg : accent),
                        const SizedBox(width: Space.xs),
                      ],
                      Expanded(child: Text(label, style: AppText.body.copyWith(color: fg))),
                      if (option.count > 0)
                        Text('${option.count}', style: AppText.bodyStrong.copyWith(color: fg.withValues(alpha: 0.8))),
                    ],
                  ),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

/// Going / Maybe / Can't go.
class _PlanChoice extends StatelessWidget {
  const _PlanChoice({
    required this.option,
    required this.fg,
    required this.accent,
    required this.mine,
    required this.onTap,
  });

  final PollOptionView option;
  final Color fg;
  final Color accent;
  final bool mine;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final on = option.mine;
    final selected = mine ? fg : accent;
    return Material(
      type: MaterialType.transparency,
      child: InkWell(
        onTap: onTap,
        borderRadius: BorderRadius.circular(Radii.sm),
        child: AnimatedContainer(
          duration: Motion.fast,
          padding: const EdgeInsets.symmetric(vertical: Space.sm, horizontal: Space.xs),
          decoration: BoxDecoration(
            color: on ? selected : Colors.transparent,
            borderRadius: BorderRadius.circular(Radii.sm),
            border: Border.all(color: on ? selected : fg.withValues(alpha: 0.22)),
          ),
          child: Column(
            children: [
              Text(
                option.label,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style:
                    AppText.caption.copyWith(color: on ? Palette.onColor(selected) : fg, fontWeight: FontWeight.w600),
              ),
              Text(
                '${option.count}',
                style: AppText.micro.copyWith(color: (on ? Palette.onColor(selected) : fg).withValues(alpha: 0.75)),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

enum _Kind { poll, dates, plan }

/// The sheet for starting a poll, a date poll or a plan in a chat.
Future<bool> showPollComposer(BuildContext context, {required int chatId, required Color accent}) async {
  final sent = await showModalBottomSheet<bool>(
    context: context,
    isScrollControlled: true,
    showDragHandle: true,
    builder: (_) => _Composer(chatId: chatId, accent: accent),
  );
  return sent ?? false;
}

class _Composer extends StatefulWidget {
  const _Composer({required this.chatId, required this.accent});

  final int chatId;
  final Color accent;

  @override
  State<_Composer> createState() => _ComposerState();
}

class _ComposerState extends State<_Composer> {
  var _kind = _Kind.poll;
  final _question = TextEditingController();
  final _place = TextEditingController();
  final _options = [TextEditingController(), TextEditingController()];
  final _dates = <DateTime>[];
  DateTime? _when;
  var _multi = false;
  var _sending = false;

  static const _max = 12;

  @override
  void dispose() {
    _question.dispose();
    _place.dispose();
    for (final o in _options) {
      o.dispose();
    }
    super.dispose();
  }

  bool get _ready {
    if (_question.text.trim().isEmpty) return false;
    return switch (_kind) {
      _Kind.poll => _options.where((o) => o.text.trim().isNotEmpty).length >= 2,
      _Kind.dates => _dates.length >= 2,
      _Kind.plan => _when != null,
    };
  }

  /// A day, then optionally a time (cancel the time for the whole day).
  Future<DateTime?> _pickWhen({DateTime? initial, bool askTime = true}) async {
    final now = DateTime.now();
    final day = await showDatePicker(
      context: context,
      initialDate: initial ?? now,
      firstDate: DateTime(now.year, now.month, now.day),
      lastDate: DateTime(now.year + 3),
    );
    if (day == null || !mounted || !askTime) return day;
    final time = await showTimePicker(
      context: context,
      initialTime: initial == null ? const TimeOfDay(hour: 19, minute: 0) : TimeOfDay.fromDateTime(initial),
      helpText: 'Pick a time, or cancel for the whole day',
    );
    if (time == null) return day;
    return DateTime(day.year, day.month, day.day, time.hour, time.minute);
  }

  Future<void> _send() async {
    if (!_ready || _sending) return;
    setState(() => _sending = true);
    try {
      switch (_kind) {
        case _Kind.poll:
          await pollSend(
            chatId: widget.chatId,
            question: _question.text.trim(),
            options: [
              for (final o in _options)
                if (o.text.trim().isNotEmpty) o.text.trim(),
            ],
            dates: Int64List(0),
            multi: _multi,
          );
        case _Kind.dates:
          _dates.sort();
          await pollSend(
            chatId: widget.chatId,
            question: _question.text.trim(),
            options: const [],
            dates: Int64List.fromList([for (final d in _dates) _toUnix(d)]),
            multi: true,
          );
        case _Kind.plan:
          await planSend(
            chatId: widget.chatId,
            title: _question.text.trim(),
            when: _toUnix(_when!),
            place: _place.text.trim(),
          );
      }
      HapticFeedback.lightImpact();
      if (mounted) Navigator.pop(context, true);
    } catch (_) {
      if (mounted) {
        setState(() => _sending = false);
        ScaffoldMessenger.of(context)
            .showSnackBar(const SnackBar(content: Text('Couldn\'t send that. Please try again.')));
      }
    }
  }

  InputDecoration _field(String hint, {IconData? icon}) => InputDecoration(
        hintText: hint,
        prefixIcon: icon == null ? null : Icon(icon, size: 20),
        filled: true,
        fillColor: Palette.surfaceHigh,
        border: OutlineInputBorder(borderRadius: BorderRadius.circular(Radii.md), borderSide: BorderSide.none),
        focusedBorder: OutlineInputBorder(
          borderRadius: BorderRadius.circular(Radii.md),
          borderSide: BorderSide(color: widget.accent.withValues(alpha: 0.6)),
        ),
      );

  @override
  Widget build(BuildContext context) {
    final accent = widget.accent;
    return Padding(
      padding: EdgeInsets.only(bottom: MediaQuery.viewInsetsOf(context).bottom),
      child: SafeArea(
        child: SingleChildScrollView(
          padding: const EdgeInsets.fromLTRB(Space.lg, 0, Space.lg, Space.lg),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            mainAxisSize: MainAxisSize.min,
            children: [
              SegmentedButton<_Kind>(
                segments: const [
                  ButtonSegment(value: _Kind.poll, label: Text('Poll'), icon: Icon(Icons.poll_outlined)),
                  ButtonSegment(value: _Kind.dates, label: Text('Pick a date'), icon: Icon(Icons.date_range_rounded)),
                  ButtonSegment(value: _Kind.plan, label: Text('Plan'), icon: Icon(Icons.event_rounded)),
                ],
                selected: {_kind},
                showSelectedIcon: false,
                onSelectionChanged: (s) => setState(() => _kind = s.first),
              ),
              const SizedBox(height: Space.md),
              Text(
                switch (_kind) {
                  _Kind.poll => 'Ask a question and let everyone choose.',
                  _Kind.dates => 'Suggest a few days or times; everyone ticks the ones that work.',
                  _Kind.plan => 'Say what, when and where; everyone answers Going, Maybe or Can\'t go.',
                },
                style: AppText.caption,
              ),
              const SizedBox(height: Space.md),
              TextField(
                controller: _question,
                autofocus: true,
                maxLength: 300,
                textCapitalization: TextCapitalization.sentences,
                cursorColor: accent,
                decoration: _field(_kind == _Kind.plan ? 'What are we doing?' : 'Question').copyWith(counterText: ''),
                onChanged: (_) => setState(() {}),
              ),
              const SizedBox(height: Space.sm),
              ...switch (_kind) {
                _Kind.poll => _pollFields(accent),
                _Kind.dates => _dateFields(accent),
                _Kind.plan => _planFields(accent),
              },
              const SizedBox(height: Space.lg),
              FilledButton(
                style: FilledButton.styleFrom(
                  backgroundColor: accent,
                  foregroundColor: Palette.onColor(accent),
                  minimumSize: const Size.fromHeight(48),
                ),
                onPressed: _ready && !_sending ? _send : null,
                child: Text(_kind == _Kind.plan ? 'Share plan' : 'Send poll'),
              ),
            ],
          ),
        ),
      ),
    );
  }

  List<Widget> _pollFields(Color accent) => [
        for (final (i, o) in _options.indexed)
          Padding(
            padding: const EdgeInsets.only(bottom: Space.sm),
            child: TextField(
              controller: o,
              maxLength: 100,
              textCapitalization: TextCapitalization.sentences,
              cursorColor: accent,
              decoration: _field('Choice ${i + 1}').copyWith(
                counterText: '',
                suffixIcon: _options.length > 2
                    ? IconButton(
                        tooltip: 'Remove',
                        icon: const Icon(Icons.close_rounded, size: 18),
                        onPressed: () => setState(() => _options.removeAt(i).dispose()),
                      )
                    : null,
              ),
              onChanged: (_) => setState(() {}),
            ),
          ),
        if (_options.length < _max)
          Align(
            alignment: Alignment.centerLeft,
            child: TextButton.icon(
              onPressed: () => setState(() => _options.add(TextEditingController())),
              icon: const Icon(Icons.add_rounded),
              label: const Text('Add a choice'),
              style: TextButton.styleFrom(foregroundColor: accent),
            ),
          ),
        SwitchListTile(
          contentPadding: EdgeInsets.zero,
          value: _multi,
          activeThumbColor: accent,
          title: const Text('Allow more than one answer'),
          onChanged: (v) => setState(() => _multi = v),
        ),
      ];

  List<Widget> _dateFields(Color accent) => [
        for (final (i, d) in _dates.indexed)
          ListTile(
            contentPadding: EdgeInsets.zero,
            leading: Icon(Icons.event_available_rounded, color: accent),
            title: Text(formatWhen(d)),
            trailing: IconButton(
              tooltip: 'Remove',
              icon: const Icon(Icons.close_rounded, size: 18),
              onPressed: () => setState(() => _dates.removeAt(i)),
            ),
          ),
        if (_dates.length < _max)
          Align(
            alignment: Alignment.centerLeft,
            child: TextButton.icon(
              onPressed: () async {
                final d = await _pickWhen(initial: _dates.isEmpty ? null : _dates.last);
                if (d != null && !_dates.contains(d)) setState(() => _dates.add(d));
              },
              icon: const Icon(Icons.add_rounded),
              label: Text(_dates.isEmpty ? 'Add a day or time' : 'Add another'),
              style: TextButton.styleFrom(foregroundColor: accent),
            ),
          ),
        if (_dates.length < 2) Text('Add at least two.', style: AppText.micro),
      ];

  List<Widget> _planFields(Color accent) => [
        ListTile(
          contentPadding: EdgeInsets.zero,
          leading: Icon(Icons.schedule_rounded, color: accent),
          title: Text(_when == null ? 'When?' : formatWhen(_when!)),
          trailing: const Icon(Icons.chevron_right_rounded),
          onTap: () async {
            final d = await _pickWhen(initial: _when);
            if (d != null) setState(() => _when = d);
          },
        ),
        TextField(
          controller: _place,
          maxLength: 200,
          textCapitalization: TextCapitalization.sentences,
          cursorColor: accent,
          decoration: _field('Where? (optional)', icon: Icons.place_outlined).copyWith(counterText: ''),
        ),
      ];
}
