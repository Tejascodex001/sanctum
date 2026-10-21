import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../background/background.dart';
import '../identity/secrets.dart';
import '../net/net_bus.dart';
import '../notify/notifier.dart';
import '../src/rust/api/feedback.dart';
import '../src/rust/api/session.dart';
import '../theme/tokens.dart';
import '../version.dart';

/// Feedback to the developer: sent privately through Tor, straight to
/// their own Sanctum, without saying who sent it. The developer's inbox
/// shows up in Settings after tapping the version 7 times.
abstract final class FeedbackInbox {
  static final unlocked = ValueNotifier(false);

  static Future<void> load() async => unlocked.value = await loadFlag('feedback_inbox', fallback: false);

  static Future<void> unlock() async {
    await saveFlag('feedback_inbox', true);
    unlocked.value = true;
  }
}

/// The Settings rows: "Send feedback", and the inbox once unlocked.
class FeedbackSettings extends StatefulWidget {
  const FeedbackSettings({super.key});

  @override
  State<FeedbackSettings> createState() => _FeedbackSettingsState();
}

class _FeedbackSettingsState extends State<FeedbackSettings> {
  @override
  void initState() {
    super.initState();
    FeedbackInbox.load();
  }

  @override
  Widget build(BuildContext context) {
    return Material(
      color: Palette.surface,
      borderRadius: BorderRadius.circular(Radii.md),
      clipBehavior: Clip.antiAlias,
      child: ValueListenableBuilder(
        valueListenable: FeedbackInbox.unlocked,
        builder: (context, unlocked, _) => Column(
          children: [
            ListTile(
              leading: const Icon(Icons.rate_review_rounded),
              title: const Text('Send feedback'),
              subtitle:
                  const Text('Tell the developer what works and what doesn\'t. Private, and nobody sees who sent it.'),
              trailing: const Icon(Icons.chevron_right_rounded),
              onTap: () => Navigator.push(context, MaterialPageRoute<void>(builder: (_) => const FeedbackScreen())),
            ),
            if (unlocked)
              ListTile(
                leading: const Icon(Icons.inbox_rounded),
                title: const Text('Feedback inbox'),
                subtitle: const Text('What people have sent you'),
                trailing: const Icon(Icons.chevron_right_rounded),
                onTap: () =>
                    Navigator.push(context, MaterialPageRoute<void>(builder: (_) => const FeedbackInboxScreen())),
              ),
          ],
        ),
      ),
    );
  }
}

/// The form: stars, what they think, and the phone's details (which they
/// can leave out). Shows exactly what will be sent.
class FeedbackScreen extends StatefulWidget {
  const FeedbackScreen({super.key});

  @override
  State<FeedbackScreen> createState() => _FeedbackScreenState();
}

class _FeedbackScreenState extends State<FeedbackScreen> {
  final _text = TextEditingController();
  int _rating = 0;
  bool _details = true;
  bool _sending = false;
  String _phone = '';
  String _android = '';
  int _build = 0;

  /// Feedback sent earlier that hasn't got through yet.
  int _waiting = 0;

  @override
  void initState() {
    super.initState();
    feedbackWaiting().then((n) {
      if (mounted) setState(() => _waiting = n);
    });
    Background.deviceInfo().then((d) {
      if (mounted) {
        setState(() {
          _phone = d.phone;
          _android = d.android;
        });
      }
    });
    Background.buildNumber().then((b) {
      if (mounted) setState(() => _build = b);
    });
    _text.addListener(() => setState(() {}));
  }

  @override
  void dispose() {
    _text.dispose();
    super.dispose();
  }

  bool get _ready => !_sending && (_rating > 0 || _text.text.trim().isNotEmpty);

  Future<void> _send() async {
    HapticFeedback.selectionClick();
    setState(() => _sending = true);
    final FeedbackSent sent;
    try {
      sent = await feedbackSend(
        form: FeedbackForm(
          phone: _details ? _phone : '',
          android: _details ? _android : '',
          build: _details ? _build : 0,
          rating: _rating,
          text: _text.text,
        ),
      );
    } catch (_) {
      if (mounted) setState(() => _sending = false);
      _say('Couldn\'t save your feedback. Please try again.');
      return;
    }
    if (!mounted) return;
    setState(() => _sending = false);
    switch (sent) {
      case FeedbackSent.queued:
        final online = NetBus.instance.status.value.status == NetStatus.online;
        _say(online
            ? 'Thank you! Your feedback is on its way.'
            : 'Thank you! Your feedback will be sent once Sanctum is connected.');
        Navigator.pop(context);
      case FeedbackSent.empty:
        _say('Pick some stars or write something first.');
      case FeedbackSent.tooMany:
        _say('You\'ve sent a lot of feedback today. Please try again tomorrow.');
      case FeedbackSent.notOpen:
        _say('Feedback isn\'t open yet in this version. It will be soon.');
    }
  }

  void _say(String text) => ScaffoldMessenger.of(context)
    ..hideCurrentSnackBar()
    ..showSnackBar(SnackBar(content: Text(text)));

  @override
  Widget build(BuildContext context) {
    final accent = Theme.of(context).colorScheme.primary;
    final open = feedbackOpen();
    return Scaffold(
      appBar: AppBar(title: const Text('Send feedback')),
      body: ListView(
        padding: EdgeInsets.fromLTRB(Space.lg, Space.sm, Space.lg, Space.xl + MediaQuery.paddingOf(context).bottom),
        children: [
          Text(
            'It goes straight to the developer\'s phone, privately through Tor. It doesn\'t carry your name, your '
            'friend code or anything that says who you are.',
            style: AppText.body.copyWith(color: Palette.textMuted),
          ),
          if (_waiting > 0) ...[
            const SizedBox(height: Space.md),
            Row(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Icon(Icons.schedule_send_rounded, size: 20, color: Palette.textMuted),
                const SizedBox(width: Space.sm),
                Expanded(
                  child: Text(
                    _waiting == 1
                        ? 'Your earlier feedback is still on its way. Sanctum keeps trying while it\'s connected.'
                        : '$_waiting pieces of your earlier feedback are still on their way. Sanctum keeps trying '
                            'while it\'s connected.',
                    style: AppText.caption,
                  ),
                ),
              ],
            ),
          ],
          if (!open) ...[
            const SizedBox(height: Space.md),
            Row(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Icon(Icons.schedule_rounded, size: 20, color: Palette.textMuted),
                const SizedBox(width: Space.sm),
                Expanded(
                    child:
                        Text('Feedback isn\'t open yet in this version. It will be soon.', style: AppText.bodyStrong)),
              ],
            ),
          ],
          const SizedBox(height: Space.xl),
          Text('HOW DO YOU LIKE SANCTUM?', style: AppText.overline),
          const SizedBox(height: Space.sm),
          Row(
            children: [
              for (var star = 1; star <= 5; star++)
                IconButton(
                  tooltip: '$star of 5',
                  iconSize: 36,
                  color: star <= _rating ? accent : Palette.textFaint,
                  icon: Icon(star <= _rating ? Icons.star_rounded : Icons.star_outline_rounded),
                  onPressed: () {
                    HapticFeedback.selectionClick();
                    setState(() => _rating = _rating == star ? 0 : star);
                  },
                ),
            ],
          ),
          const SizedBox(height: Space.lg),
          Text('YOUR FEEDBACK', style: AppText.overline),
          const SizedBox(height: Space.sm),
          TextField(
            controller: _text,
            minLines: 5,
            maxLines: 12,
            maxLength: 4000,
            textCapitalization: TextCapitalization.sentences,
            decoration: const InputDecoration(
              hintText: 'What do you like? What\'s confusing or broken? What would you add?',
            ),
          ),
          const SizedBox(height: Space.md),
          Material(
            color: Palette.surface,
            borderRadius: BorderRadius.circular(Radii.md),
            clipBehavior: Clip.antiAlias,
            child: Column(
              children: [
                SwitchListTile(
                  value: _details,
                  title: const Text('Include phone details'),
                  subtitle: const Text('Helps the developer fix problems on phones like yours'),
                  onChanged: (v) => setState(() => _details = v),
                ),
                Padding(
                  padding: const EdgeInsets.fromLTRB(Space.lg, 0, Space.lg, Space.lg),
                  child: _Sent(
                    phone: _details ? _phone : null,
                    android: _details ? _android : null,
                    buildNumber: _details ? _build : null,
                    rating: _rating,
                  ),
                ),
              ],
            ),
          ),
          const SizedBox(height: Space.xl),
          FilledButton.icon(
            onPressed: open && _ready ? _send : null,
            icon: _sending
                ? const SizedBox.square(dimension: 18, child: CircularProgressIndicator(strokeWidth: 2))
                : const Icon(Icons.send_rounded),
            label: const Text('Send'),
          ),
        ],
      ),
    );
  }
}

/// "What will be sent", line by line.
class _Sent extends StatelessWidget {
  const _Sent({required this.phone, required this.android, required this.buildNumber, required this.rating});

  final String? phone;
  final String? android;
  final int? buildNumber;
  final int rating;

  @override
  Widget build(BuildContext context) {
    final lines = [
      ('Phone', phone == null ? 'Not included' : (phone!.isEmpty ? 'Unknown' : phone!)),
      ('Android version', android == null ? 'Not included' : (android!.isEmpty ? 'Unknown' : android!)),
      ('Sanctum version', buildNumber == null ? 'Not included' : fullVersion(buildNumber!)),
      ('Rating', rating == 0 ? 'No stars' : '$rating of 5 stars'),
      ('Feedback', 'What you wrote above'),
    ];
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text('WHAT WILL BE SENT', style: AppText.overline),
        const SizedBox(height: Space.sm),
        for (final (label, value) in lines)
          Padding(
            padding: const EdgeInsets.only(bottom: Space.xs),
            child: Row(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                SizedBox(width: 130, child: Text(label, style: AppText.caption)),
                Expanded(child: Text(value, style: AppText.caption.copyWith(color: Palette.text))),
              ],
            ),
          ),
      ],
    );
  }
}

/// The developer's inbox: the switch that takes feedback in, this phone's
/// feedback address, and what arrived.
class FeedbackInboxScreen extends StatefulWidget {
  const FeedbackInboxScreen({super.key, @visibleForTesting this.sample});

  /// Shown instead of the inbox (screenshots in tests).
  final List<FeedbackItem>? sample;

  @override
  State<FeedbackInboxScreen> createState() => _FeedbackInboxScreenState();
}

class _FeedbackInboxScreenState extends State<FeedbackInboxScreen> {
  bool _receiving = false;
  String _address = '';
  List<FeedbackItem> _items = const [];
  FeedbackCheck? _check;

  // Filters: stars (null: any, 0: none given), version (null: any), only
  // ones with words, a search, and the order.
  int? _stars;
  int? _build;
  bool _withText = false;
  String _query = '';
  bool _oldestFirst = false;
  bool _testing = false;
  String? _testResult;
  StreamSubscription<AppEvent>? _arrivals;

  @override
  void initState() {
    super.initState();
    _load();
    _arrivals = NetBus.instance.events.where((e) => e.kind == AppEventKind.feedback).listen((_) => _load());
  }

  @override
  void dispose() {
    _arrivals?.cancel();
    super.dispose();
  }

  Future<void> _load() async {
    Notifier.instance.feedbackSeen();
    final receiving = await feedbackReceiving();
    final items = widget.sample ?? await feedbackInbox();
    final address = receiving ? await feedbackAddress() : '';
    final check = await feedbackCheck();
    if (!mounted) return;
    setState(() {
      _receiving = receiving;
      _items = items;
      _address = address;
      _check = check;
    });
  }

  bool get _filtering => _stars != null || _build != null || _withText || _query.isNotEmpty;

  /// What the filters let through, in the chosen order.
  List<FeedbackItem> get _shown {
    final query = _query.trim().toLowerCase();
    final shown = _items.where((i) {
      if (_stars != null && i.rating != _stars) return false;
      if (_build != null && i.build.toInt() != _build) return false;
      if (_withText && i.text.trim().isEmpty) return false;
      if (query.isNotEmpty && !'${i.text} ${i.phone} ${i.android}'.toLowerCase().contains(query)) return false;
      return true;
    }).toList();
    return _oldestFirst ? shown.reversed.toList() : shown;
  }

  void _clearFilters() => setState(() {
        _stars = null;
        _build = null;
        _withText = false;
        _query = '';
      });

  /// Sends a test to this phone's own address through Tor.
  Future<void> _test() async {
    HapticFeedback.selectionClick();
    setState(() {
      _testing = true;
      _testResult = null;
    });
    final failure = await feedbackTest();
    if (!mounted) return;
    setState(() {
      _testing = false;
      _testResult =
          failure.isEmpty ? 'The test arrived: feedback works on this phone.' : 'The test didn\'t arrive: $failure';
    });
    await _load();
  }

  /// Everything the check shows, as text to send to the developer's helper.
  String _report() {
    final c = _check;
    if (c == null) return '';
    return [
      'Sanctum feedback check',
      'Connected: ${c.online}',
      'Receiving: ${c.receiving}',
      'Service: ${c.service}',
      'Address matches: ${c.addressOk}',
      'This version sends here: ${c.sendsHere}',
      'Waiting to send: ${c.waiting}',
      'Last send: ${c.lastTry.isEmpty ? 'none' : c.lastTry}',
      'Problem: ${c.problem.isEmpty ? 'none' : c.problem}',
      if (_testResult != null) 'Test: $_testResult',
    ].join('\n');
  }

  Future<void> _setReceiving(bool on) async {
    HapticFeedback.selectionClick();
    try {
      await feedbackSetReceiving(enabled: on);
    } catch (_) {
      _say('Couldn\'t change that. Please try again.');
    }
    await _load();
  }

  Future<void> _delete(FeedbackItem item) async {
    await feedbackDelete(item: item);
    await _load();
  }

  void _say(String text) {
    if (!mounted) return;
    ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(SnackBar(content: Text(text)));
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: const Text('Feedback inbox'),
        actions: [
          IconButton(
            tooltip: 'Refresh',
            icon: const Icon(Icons.refresh_rounded),
            onPressed: () {
              HapticFeedback.selectionClick();
              _load();
            },
          ),
        ],
      ),
      body: RefreshIndicator(
        onRefresh: _load,
        child: ListView(
          physics: const AlwaysScrollableScrollPhysics(),
          padding: EdgeInsets.fromLTRB(Space.lg, Space.sm, Space.lg, Space.xl + MediaQuery.paddingOf(context).bottom),
          children: [
            Material(
              color: Palette.surface,
              borderRadius: BorderRadius.circular(Radii.md),
              clipBehavior: Clip.antiAlias,
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  SwitchListTile(
                    value: _receiving,
                    title: const Text('Receive feedback'),
                    subtitle: const Text('This phone takes in what people send from "Send feedback"'),
                    onChanged: _setReceiving,
                  ),
                  if (_receiving)
                    Padding(
                      padding: const EdgeInsets.fromLTRB(Space.lg, 0, Space.lg, Space.lg),
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          Text('YOUR FEEDBACK ADDRESS', style: AppText.overline),
                          const SizedBox(height: Space.xs),
                          SelectableText(_address, style: AppText.mono.copyWith(fontSize: 13)),
                          const SizedBox(height: Space.sm),
                          Text(
                            'Builds of Sanctum send feedback here. It can only receive, and isn\'t your friend code. '
                            'Keep this phone connected now and then: feedback waits on the sender\'s phone until it '
                            'gets through.',
                            style: AppText.caption,
                          ),
                          const SizedBox(height: Space.sm),
                          OutlinedButton.icon(
                            onPressed: () {
                              Clipboard.setData(ClipboardData(text: _address));
                              _say('Address copied.');
                            },
                            icon: const Icon(Icons.copy_rounded, size: 18),
                            label: const Text('Copy address'),
                          ),
                        ],
                      ),
                    ),
                ],
              ),
            ),
            if (_check != null) ...[
              const SizedBox(height: Space.xl),
              Text('CHECK', style: AppText.overline),
              const SizedBox(height: Space.sm),
              _CheckPanel(
                check: _check!,
                testing: _testing,
                testResult: _testResult,
                onTest: _receiving && !_testing ? _test : null,
                onRefresh: _load,
                onCopy: () {
                  Clipboard.setData(ClipboardData(text: _report()));
                  _say('Report copied.');
                },
              ),
            ],
            const SizedBox(height: Space.xl),
            if (_items.isEmpty)
              Text('NOTHING YET', style: AppText.overline)
            else ...[
              _Filters(
                items: _items,
                stars: _stars,
                version: _build,
                withText: _withText,
                oldestFirst: _oldestFirst,
                onStars: (v) => setState(() => _stars = v),
                onBuild: (v) => setState(() => _build = v),
                onWithText: (v) => setState(() => _withText = v),
                onOldestFirst: (v) => setState(() => _oldestFirst = v),
                onQuery: (v) => setState(() => _query = v),
              ),
              const SizedBox(height: Space.md),
              _Summary(shown: _shown, total: _items.length, filtering: _filtering, onClear: _clearFilters),
              const SizedBox(height: Space.sm),
              for (final item in _shown) _FeedbackCard(item: item, onDelete: () => _delete(item)),
            ],
          ],
        ),
      ),
    );
  }
}

/// Search, stars, version, "has words" and the order.
class _Filters extends StatelessWidget {
  const _Filters({
    required this.items,
    required this.stars,
    required this.version,
    required this.withText,
    required this.oldestFirst,
    required this.onStars,
    required this.onBuild,
    required this.onWithText,
    required this.onOldestFirst,
    required this.onQuery,
  });

  final List<FeedbackItem> items;
  final int? stars;
  final int? version;
  final bool withText;
  final bool oldestFirst;
  final ValueChanged<int?> onStars;
  final ValueChanged<int?> onBuild;
  final ValueChanged<bool> onWithText;
  final ValueChanged<bool> onOldestFirst;
  final ValueChanged<String> onQuery;

  @override
  Widget build(BuildContext context) {
    final builds = {for (final i in items) i.build.toInt()}.toList()..sort((a, b) => b.compareTo(a));
    Widget chip(String label, bool selected, VoidCallback onTap) => Padding(
          padding: const EdgeInsets.only(right: Space.sm),
          child: ChoiceChip(label: Text(label), selected: selected, onSelected: (_) => onTap()),
        );
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        TextField(
          onChanged: onQuery,
          textInputAction: TextInputAction.search,
          decoration: const InputDecoration(
            prefixIcon: Icon(Icons.search_rounded),
            hintText: 'Search what people wrote, or their phone',
          ),
        ),
        const SizedBox(height: Space.md),
        SingleChildScrollView(
          scrollDirection: Axis.horizontal,
          child: Row(
            children: [
              chip('All stars', stars == null, () => onStars(null)),
              for (var n = 5; n >= 1; n--)
                chip(n == 1 ? '1 star' : '$n stars', stars == n, () => onStars(stars == n ? null : n)),
              chip('No stars', stars == 0, () => onStars(stars == 0 ? null : 0)),
            ],
          ),
        ),
        const SizedBox(height: Space.sm),
        SingleChildScrollView(
          scrollDirection: Axis.horizontal,
          child: Row(
            children: [
              Padding(
                padding: const EdgeInsets.only(right: Space.sm),
                child: PopupMenuButton<int?>(
                  tooltip: 'Version',
                  onSelected: (v) => onBuild(v == -1 ? null : v),
                  itemBuilder: (_) => [
                    const PopupMenuItem(value: -1, child: Text('All versions')),
                    for (final b in builds) PopupMenuItem(value: b, child: Text(b == 0 ? 'Not given' : fullVersion(b))),
                  ],
                  child: Chip(
                    avatar: const Icon(Icons.expand_more_rounded, size: 18),
                    label: Text(version == null
                        ? 'All versions'
                        : (version == 0 ? 'Version not given' : fullVersion(version!))),
                  ),
                ),
              ),
              Padding(
                padding: const EdgeInsets.only(right: Space.sm),
                child: FilterChip(label: const Text('Has words'), selected: withText, onSelected: onWithText),
              ),
              ChoiceChip(
                avatar: const Icon(Icons.swap_vert_rounded, size: 18),
                label: Text(oldestFirst ? 'Oldest first' : 'Newest first'),
                selected: false,
                onSelected: (_) => onOldestFirst(!oldestFirst),
              ),
            ],
          ),
        ),
      ],
    );
  }
}

/// "12 of 40 shown · average 4.2 stars", and a way back to everything.
class _Summary extends StatelessWidget {
  const _Summary({required this.shown, required this.total, required this.filtering, required this.onClear});

  final List<FeedbackItem> shown;
  final int total;
  final bool filtering;
  final VoidCallback onClear;

  @override
  Widget build(BuildContext context) {
    final rated = shown.where((i) => i.rating > 0).toList();
    final average = rated.isEmpty ? null : rated.map((i) => i.rating).reduce((a, b) => a + b) / rated.length;
    final count = filtering ? '${shown.length} OF $total SHOWN' : '$total RECEIVED';
    return Row(
      children: [
        Expanded(
          child: Text(
            average == null ? count : '$count · AVERAGE ${average.toStringAsFixed(1)} STARS',
            style: AppText.overline,
          ),
        ),
        if (filtering) TextButton(onPressed: onClear, child: const Text('Clear filters')),
      ],
    );
  }
}

/// How feedback stands on this phone, in plain words, with a test.
class _CheckPanel extends StatelessWidget {
  const _CheckPanel({
    required this.check,
    required this.testing,
    required this.testResult,
    required this.onTest,
    required this.onRefresh,
    required this.onCopy,
  });

  final FeedbackCheck check;
  final bool testing;
  final String? testResult;
  final VoidCallback? onTest;
  final VoidCallback onRefresh;
  final VoidCallback onCopy;

  @override
  Widget build(BuildContext context) {
    final service = switch (check.service) {
      'ready' => 'Ready',
      'starting' => 'Getting ready (can take a few minutes)',
      'trouble' => 'Having trouble, still trying',
      'broken' => 'Not working',
      'stopped' => 'Stopped',
      _ => check.receiving ? 'Not started' : 'Off',
    };
    final rows = [
      ('Connected', check.online ? 'Yes' : 'Not yet', check.online),
      ('Receiving', service, check.service == 'ready'),
      ('Address', check.service == 'off' ? '–' : (check.addressOk ? 'Matches' : 'Doesn\'t match'), check.addressOk),
      ('This version sends here', check.sendsHere ? 'Yes' : 'No', check.sendsHere),
      ('Waiting to send', '${check.waiting}', check.waiting == 0),
    ];
    return Container(
      padding: const EdgeInsets.all(Space.lg),
      decoration: BoxDecoration(
        color: Palette.surface,
        borderRadius: BorderRadius.circular(Radii.md),
        border: Border.all(color: Palette.outline),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          for (final (label, value, good) in rows)
            Padding(
              padding: const EdgeInsets.only(bottom: Space.xs),
              child: Row(
                children: [
                  Icon(
                    good ? Icons.check_circle_rounded : Icons.error_outline_rounded,
                    size: 16,
                    color: good ? Palette.textMuted : Palette.warning,
                  ),
                  const SizedBox(width: Space.sm),
                  SizedBox(width: 150, child: Text(label, style: AppText.caption)),
                  Expanded(child: Text(value, style: AppText.caption.copyWith(color: Palette.text))),
                ],
              ),
            ),
          if (check.lastTry.isNotEmpty) ...[
            const SizedBox(height: Space.sm),
            Text('Last send: ${check.lastTry}', style: AppText.caption),
          ],
          if (check.problem.isNotEmpty) ...[
            const SizedBox(height: Space.sm),
            SelectableText(check.problem, style: AppText.mono.copyWith(fontSize: 11, color: Palette.textMuted)),
          ],
          if (testResult != null) ...[
            const SizedBox(height: Space.md),
            Text(testResult!, style: AppText.bodyStrong),
          ],
          const SizedBox(height: Space.md),
          Wrap(
            spacing: Space.sm,
            runSpacing: Space.sm,
            children: [
              FilledButton.icon(
                onPressed: onTest,
                icon: testing
                    ? const SizedBox.square(dimension: 16, child: CircularProgressIndicator(strokeWidth: 2))
                    : const Icon(Icons.send_rounded, size: 18),
                label: Text(testing ? 'Testing…' : 'Send a test'),
              ),
              OutlinedButton(onPressed: onRefresh, child: const Text('Refresh')),
              OutlinedButton(onPressed: onCopy, child: const Text('Copy report')),
            ],
          ),
        ],
      ),
    );
  }
}

class _FeedbackCard extends StatelessWidget {
  const _FeedbackCard({required this.item, required this.onDelete});

  final FeedbackItem item;
  final VoidCallback onDelete;

  @override
  Widget build(BuildContext context) {
    final accent = Theme.of(context).colorScheme.primary;
    final when = DateTime.fromMillisecondsSinceEpoch(item.at.toInt() * 1000);
    final details = [
      if (item.phone.isNotEmpty) item.phone,
      if (item.android.isNotEmpty) 'Android ${item.android}',
      if (item.build > 0) fullVersion(item.build.toInt()),
    ].join(' · ');
    return Container(
      margin: const EdgeInsets.only(bottom: Space.md),
      padding: const EdgeInsets.fromLTRB(Space.lg, Space.md, Space.sm, Space.lg),
      decoration: BoxDecoration(
        color: Palette.surface,
        borderRadius: BorderRadius.circular(Radii.md),
        border: Border.all(color: Palette.outline),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              for (var star = 1; star <= 5; star++)
                Icon(
                  star <= item.rating ? Icons.star_rounded : Icons.star_outline_rounded,
                  size: 18,
                  color: star <= item.rating ? accent : Palette.textFaint,
                ),
              const SizedBox(width: Space.sm),
              Expanded(child: Text(_date(when), style: AppText.caption)),
              IconButton(
                tooltip: 'Delete',
                icon: const Icon(Icons.delete_outline_rounded, size: 20),
                onPressed: onDelete,
              ),
            ],
          ),
          if (item.text.isNotEmpty) ...[
            const SizedBox(height: Space.xs),
            Padding(
              padding: const EdgeInsets.only(right: Space.sm),
              child: SelectableText(item.text, style: AppText.body),
            ),
          ],
          const SizedBox(height: Space.sm),
          Text(details.isEmpty ? 'No phone details' : details, style: AppText.caption),
        ],
      ),
    );
  }

  static String _date(DateTime t) {
    const months = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
    final minute = t.minute.toString().padLeft(2, '0');
    return '${t.day} ${months[t.month - 1]} ${t.year}, ${t.hour}:$minute';
  }
}
