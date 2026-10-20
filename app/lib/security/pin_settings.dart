import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../identity/secrets.dart';
import '../theme/tokens.dart';
import 'profile.dart';

/// The Sanctum PIN, and the decoy PIN that opens an empty-looking
/// Sanctum. In the decoy profile only the PIN row shows, so the settings
/// don't give the decoy away.
class PinSettings extends StatefulWidget {
  const PinSettings({super.key});

  @override
  State<PinSettings> createState() => _PinSettingsState();
}

class _PinSettingsState extends State<PinSettings> {
  bool? _pin;
  bool _decoy = false;

  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load() async {
    final pin = await hasPin();
    final decoy = await hasDecoyPin();
    if (!mounted) return;
    setState(() {
      _pin = pin;
      _decoy = decoy;
    });
  }

  void _toast(String text) => ScaffoldMessenger.of(context)
    ..hideCurrentSnackBar()
    ..showSnackBar(SnackBar(content: Text(text)));

  Future<void> _setPin() async {
    final pin = await askNewPin(context, title: _pin == true ? 'New Sanctum PIN' : 'Set a Sanctum PIN');
    if (pin == null) return;
    if (await checkPin(pin) == PinMatch.decoy) return _toast('That\'s your decoy PIN; pick another.');
    await setPin(pin);
    HapticFeedback.mediumImpact();
    _toast('PIN set. Sanctum now asks for it (fingerprint still works).');
    await _load();
  }

  Future<void> _removePin() async {
    final ok = await _confirm(
        'Remove the PIN?', 'Sanctum goes back to your fingerprint or screen lock. The decoy PIN is removed too.');
    if (!ok) return;
    await removePin();
    await _load();
  }

  Future<void> _setDecoy() async {
    final pin = await askNewPin(context, title: 'Set a decoy PIN');
    if (pin == null) return;
    if (await checkPin(pin) == PinMatch.real) return _toast('That\'s your real PIN; pick a different one.');
    await setDecoyPin(pin);
    await Profile.prepareDecoy();
    HapticFeedback.mediumImpact();
    _toast('Decoy PIN set.');
    await _load();
  }

  Future<void> _removeDecoy() async {
    final ok = await _confirm('Remove the decoy PIN?', 'It stops working. What you wrote in the decoy stays, unseen.');
    if (!ok) return;
    await removeDecoyPin();
    await _load();
  }

  Future<bool> _confirm(String title, String body) async =>
      await showDialog<bool>(
        context: context,
        builder: (context) => AlertDialog(
          title: Text(title),
          content: Text(body),
          actions: [
            TextButton(onPressed: () => Navigator.pop(context, false), child: const Text('Cancel')),
            TextButton(onPressed: () => Navigator.pop(context, true), child: const Text('Remove')),
          ],
        ),
      ) ??
      false;

  @override
  Widget build(BuildContext context) {
    final pin = _pin;
    if (pin == null) return const SizedBox(height: 64);
    final decoyProfile = Profile.decoy.value;
    return Container(
      decoration: BoxDecoration(
        color: Palette.bg.withValues(alpha: 0.4),
        borderRadius: BorderRadius.circular(Radii.md),
      ),
      child: Column(
        children: [
          ListTile(
            leading: const Icon(Icons.pin_rounded),
            title: Text(pin ? 'Sanctum PIN: on' : 'Sanctum PIN'),
            subtitle: Text(pin
                ? 'Asked every time Sanctum opens; your fingerprint still works.'
                : 'A PIN of its own for Sanctum (needed for a decoy PIN).'),
            trailing: pin && !decoyProfile
                ? IconButton(tooltip: 'Remove PIN', icon: const Icon(Icons.close_rounded), onPressed: _removePin)
                : null,
            onTap: decoyProfile ? null : _setPin,
          ),
          if (pin && !decoyProfile)
            ListTile(
              leading: const Icon(Icons.theater_comedy_rounded),
              title: Text(_decoy ? 'Decoy PIN: on' : 'Decoy PIN'),
              subtitle: const Text(
                'Opens an empty-looking Sanctum, for when you\'re made to unlock it. Your real chats stay '
                'hidden and silent until you unlock with your real PIN or fingerprint.',
              ),
              trailing: _decoy
                  ? IconButton(
                      tooltip: 'Remove decoy PIN', icon: const Icon(Icons.close_rounded), onPressed: _removeDecoy)
                  : null,
              onTap: _setDecoy,
            ),
        ],
      ),
    );
  }
}

/// Asks for a new 4–8 digit PIN twice; null if cancelled.
Future<String?> askNewPin(BuildContext context, {required String title}) async {
  final first = await _askPin(context, title: title, hint: '4 to 8 digits');
  if (first == null || !context.mounted) return null;
  final again = await _askPin(context, title: 'Repeat it', hint: 'The same PIN again');
  if (again == null || !context.mounted) return null;
  if (again != first) {
    ScaffoldMessenger.of(context).showSnackBar(const SnackBar(content: Text('The PINs didn\'t match.')));
    return null;
  }
  return first;
}

Future<String?> _askPin(BuildContext context, {required String title, required String hint}) {
  final controller = TextEditingController();
  return showDialog<String>(
    context: context,
    builder: (context) => StatefulBuilder(
      builder: (context, setDialog) {
        final ok = RegExp(r'^\d{4,8}$').hasMatch(controller.text);
        return AlertDialog(
          title: Text(title),
          content: TextField(
            controller: controller,
            autofocus: true,
            obscureText: true,
            keyboardType: TextInputType.number,
            inputFormatters: [FilteringTextInputFormatter.digitsOnly, LengthLimitingTextInputFormatter(8)],
            onChanged: (_) => setDialog(() {}),
            onSubmitted: (v) => ok ? Navigator.pop(context, v) : null,
            decoration: InputDecoration(hintText: hint),
          ),
          actions: [
            TextButton(onPressed: () => Navigator.pop(context), child: const Text('Cancel')),
            TextButton(onPressed: ok ? () => Navigator.pop(context, controller.text) : null, child: const Text('Next')),
          ],
        );
      },
    ),
  );
}
