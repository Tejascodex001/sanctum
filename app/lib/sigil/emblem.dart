import 'package:flutter/material.dart';

import '../src/rust/api/sigil.dart';
import '../theme/tokens.dart';
import 'sigil_look.dart';
import 'sigil_painter.dart';

/// A chat's sigil as its face: round for a friend, a rounded square with a
/// small badge for a group, so the two never look alike. An optional ring
/// says something is waiting (a friend's new moment).
class ChatEmblem extends StatelessWidget {
  const ChatEmblem({super.key, required this.sigil, this.group = false, this.size = 46, this.ring, this.border = true});

  final SigilSpec sigil;
  final bool group;
  final double size;

  /// Drawn around the emblem when set.
  final Color? ring;

  /// The thin accent outline (list rows); off in headers.
  final bool border;

  @override
  Widget build(BuildContext context) {
    final accent = sigil.look.accent.color;
    final radius = BorderRadius.circular(size * 0.3);
    final shape = group ? BoxShape.rectangle : BoxShape.circle;
    final edge = ring ?? (border ? accent.withValues(alpha: 0.55) : null);
    final face = group
        ? ClipRRect(borderRadius: radius, child: SigilView(spec: sigil))
        : ClipOval(child: SigilView(spec: sigil));
    final badge = size * 0.36;
    return SizedBox.square(
      dimension: size,
      child: Stack(
        clipBehavior: Clip.none,
        children: [
          Positioned.fill(
            child: AnimatedContainer(
              duration: Motion.medium,
              padding: EdgeInsets.all(ring != null ? 2.5 : (edge != null ? 1.5 : 0)),
              decoration: BoxDecoration(
                shape: shape,
                borderRadius: group ? BorderRadius.circular(size * 0.3 + 2) : null,
                border: edge == null ? null : Border.all(color: edge, width: ring != null ? 2 : 1.2),
              ),
              child: face,
            ),
          ),
          if (group)
            Positioned(
              right: -badge * 0.22,
              bottom: -badge * 0.22,
              child: Container(
                width: badge,
                height: badge,
                decoration: BoxDecoration(
                  color: accent,
                  shape: BoxShape.circle,
                  border: Border.all(color: Palette.bg, width: 1.5),
                ),
                child: Icon(Icons.group_rounded, size: badge * 0.62, color: Palette.onColor(accent)),
              ),
            ),
        ],
      ),
    );
  }
}
