import 'dart:async';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import '../background/background.dart';
import '../identity/secrets.dart';
import '../net/net_bus.dart';
import '../update/updater.dart';
import '../version.dart';
import 'feedback_screen.dart';
import '../theme/tokens.dart';

class BackgroundSettings extends StatefulWidget {
  const BackgroundSettings({super.key});

  @override
  State<BackgroundSettings> createState() => _BackgroundSettingsState();
}

class _BackgroundSettingsState extends State<BackgroundSettings> with WidgetsBindingObserver {
  bool? _on;
  bool _unrestricted = true;
  bool _canNotify = true;
  ({String brand, bool aggressive}) _brand = (brand: '', aggressive: false);

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    _load();
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    super.dispose();
  }

  // Back from the system battery dialog.
  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state == AppLifecycleState.resumed) _load();
  }

  Future<void> _load() async {
    final on = await Background.enabled();
    final unrestricted = await Background.batteryUnrestricted();
    final canNotify = await Background.canNotify();
    final brand = await Background.brand();
    if (!mounted) return;
    setState(() {
      _brand = brand;
      _on = on;
      _unrestricted = unrestricted;
      _canNotify = canNotify;
    });
  }

  @override
  Widget build(BuildContext context) {
    final on = _on;
    if (on == null) return const SizedBox(height: 64);
    return Material(
      color: Palette.surface,
      borderRadius: BorderRadius.circular(Radii.md),
      clipBehavior: Clip.antiAlias,
      child: Column(
        children: [
          SwitchListTile(
            value: on,
            title: const Text('Stay connected when closed'),
            subtitle: Text(
              on
                  ? 'Messages and requests arrive and notify you. A quiet "Connected privately" notice stays while it runs.'
                  : 'Messages wait on your friends\' phones until you open Sanctum.',
              style: AppText.caption,
            ),
            onChanged: (v) async {
              HapticFeedback.selectionClick();
              setState(() => _on = v);
              await Background.setEnabled(v);
              if (v) await Background.requestNotifications();
              await _load();
            },
          ),
          if (!_canNotify)
            const ListTile(
              dense: true,
              leading: Icon(Icons.notifications_off_outlined, color: Palette.warning),
              title: Text('Notifications are off'),
              subtitle: Text('Tap to turn them on, so you hear about new messages.'),
              onTap: Background.openNotificationSettings,
            ),
          if (on && !_unrestricted)
            ListTile(
              dense: true,
              leading: const Icon(Icons.battery_alert_rounded, color: Palette.warning),
              title: const Text('Allow unrestricted battery'),
              subtitle: const Text('Some phones cut background connections to save battery. Tap to allow.'),
              onTap: Background.requestUnrestricted,
            ),
          if (on)
            ListTile(
              dense: true,
              leading: const Icon(Icons.settings_suggest_rounded),
              title: Text(_brand.aggressive ? 'Let Sanctum run on ${brandName(_brand.brand)}' : 'Background settings'),
              subtitle: Text(
                _brand.aggressive
                    ? '${brandName(_brand.brand)} phones stop apps in the background. Opens the setting directly: '
                        'turn on Autostart / allow background activity for Sanctum.'
                    : 'Opens Sanctum\'s settings, if notifications stop after a while.',
              ),
              trailing: const Icon(Icons.open_in_new_rounded, size: 18),
              onTap: Background.openBackgroundSettings,
            ),
        ],
      ),
    );
  }
}

String brandName(String brand) => switch (brand) {
      'xiaomi' => 'Xiaomi',
      'oppo' => 'Oppo / Realme',
      'vivo' => 'Vivo',
      'huawei' => 'Huawei / Honor',
      'samsung' => 'Samsung',
      'oneplus' => 'OnePlus',
      _ => brand.isEmpty ? 'this phone' : brand[0].toUpperCase() + brand.substring(1),
    };

/// Shown until background delivery is set up to be reliable on this phone:
/// one tap opens the exact setting.

class NearbySettings extends StatefulWidget {
  const NearbySettings({super.key});

  @override
  State<NearbySettings> createState() => _NearbySettingsState();
}

class _NearbySettingsState extends State<NearbySettings> {
  bool? _on;
  String? _error;

  @override
  void initState() {
    super.initState();
    Background.nearbyEnabled().then((on) {
      if (mounted) setState(() => _on = on);
    });
  }

  Future<void> _set(bool on) async {
    HapticFeedback.selectionClick();
    setState(() {
      _on = on;
      _error = null;
    });
    try {
      await NetBus.instance.setNearby(on);
      await saveFlag('nearby', on);
    } catch (e) {
      await NetBus.instance.setNearby(false).catchError((_) {});
      if (!mounted) return;
      setState(() {
        _on = false;
        _error = 'Nearby couldn\'t start on this phone. Try turning it off and on again.';
      });
    }
  }

  @override
  Widget build(BuildContext context) {
    final on = _on;
    if (on == null) return const SizedBox(height: 64);
    return Material(
      color: Palette.surface,
      borderRadius: BorderRadius.circular(Radii.md),
      clipBehavior: Clip.antiAlias,
      child: SwitchListTile(
        value: on,
        secondary: const Icon(Icons.wifi_tethering_rounded),
        title: const Text('Nearby, without internet'),
        subtitle: ValueListenableBuilder<bool>(
          valueListenable: NetBus.instance.bluetooth,
          builder: (context, bluetooth, _) => Text(
            _error ??
                'Chat with friends on the same Wi-Fi, a hotspot, or over Bluetooth, even offline. '
                    'Over Wi-Fi they can see your local network address (never your internet one).'
                    '${on ? (bluetooth ? '\nBluetooth: on' : '\nBluetooth: off (turn Bluetooth on and allow Nearby devices; needs Android 10 or newer)') : ''}',
            style: AppText.caption.copyWith(color: _error == null ? null : Palette.danger),
          ),
        ),
        onChanged: _set,
      ),
    );
  }
}

/// "Build 27 is available": one tap downloads it through Tor and opens
/// Android's installer.

class UpdateSettings extends StatefulWidget {
  const UpdateSettings({super.key});

  @override
  State<UpdateSettings> createState() => _UpdateSettingsState();
}

class _UpdateSettingsState extends State<UpdateSettings> {
  bool _checking = false;
  bool _auto = true;

  /// Taps on the version: 7 unlock the developer's feedback inbox.
  int _taps = 0;

  void _tapVersion() {
    if (FeedbackInbox.unlocked.value || ++_taps < 7) return;
    HapticFeedback.mediumImpact();
    FeedbackInbox.unlock();
    ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(const SnackBar(content: Text('Feedback inbox is now in Settings, under Feedback.')));
  }

  @override
  void initState() {
    super.initState();
    loadFlag('auto_update_check', fallback: true).then((v) {
      if (mounted) setState(() => _auto = v);
    });
    Background.buildNumber().then((b) {
      if (mounted) setState(() => Updater.instance.current = b);
    });
  }

  Future<void> _check() async {
    HapticFeedback.selectionClick();
    setState(() => _checking = true);
    await Updater.instance.check(force: true);
    if (!mounted) return;
    setState(() => _checking = false);
    final u = Updater.instance;
    ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(SnackBar(
        content: Text(u.error.value ??
            (u.available.value == null
                ? 'Sanctum is up to date.'
                : 'A new version is ready: tap Update on the home screen.')),
      ));
  }

  @override
  Widget build(BuildContext context) {
    final current = Updater.instance.current;
    return Material(
      color: Palette.surface,
      borderRadius: BorderRadius.circular(Radii.md),
      clipBehavior: Clip.antiAlias,
      child: Column(
        children: [
          ListTile(
            leading: const Icon(Icons.system_update_rounded),
            onTap: _tapVersion,
            title: Text('Sanctum $appVersion'),
            subtitle: Text('${current == 0 ? '' : 'Build $current. '}'
                'Updates are downloaded privately, and only genuine Sanctum updates can be installed.'),
            trailing: _checking
                ? const SizedBox.square(dimension: 20, child: CircularProgressIndicator(strokeWidth: 2))
                : TextButton(onPressed: _check, child: const Text('Check')),
          ),
          SwitchListTile(
            value: _auto,
            title: const Text('Check automatically'),
            onChanged: (v) {
              setState(() => _auto = v);
              saveFlag('auto_update_check', v);
            },
          ),
        ],
      ),
    );
  }
}
