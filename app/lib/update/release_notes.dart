import 'package:flutter/material.dart';

import '../src/rust/api/update.dart';
import '../theme/tokens.dart';
import 'updater.dart';

/// What changed since the installed build, then Update.
Future<void> showReleaseNotes(BuildContext context, UpdateInfo update) => showModalBottomSheet<void>(
      context: context,
      isScrollControlled: true,
      showDragHandle: true,
      builder: (sheet) => ConstrainedBox(
        constraints: BoxConstraints(maxHeight: MediaQuery.sizeOf(sheet).height * 0.85),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Padding(
              padding: const EdgeInsets.fromLTRB(Space.xl, 0, Space.xl, Space.sm),
              child: Text('What\'s new', style: AppText.title),
            ),
            Flexible(
              child: ListView(
                shrinkWrap: true,
                padding: const EdgeInsets.fromLTRB(Space.xl, 0, Space.xl, Space.md),
                children: [
                  if (update.notes.isEmpty)
                    const Text('Improvements and fixes.', style: AppText.caption),
                  for (final notes in update.notes) ...[
                    if (update.notes.length > 1)
                      Padding(
                        padding: const EdgeInsets.only(top: Space.md, bottom: Space.xs),
                        child: Text('VERSION ${notes.build}', style: AppText.overline),
                      ),
                    ..._lines(notes.text),
                  ],
                ],
              ),
            ),
            SafeArea(
              child: Padding(
                padding: const EdgeInsets.fromLTRB(Space.xl, Space.sm, Space.xl, Space.md),
                child: FilledButton.icon(
                  onPressed: () {
                    Navigator.pop(sheet);
                    Updater.instance.install();
                  },
                  icon: const Icon(Icons.system_update_rounded),
                  label: const Text('Update'),
                ),
              ),
            ),
          ],
        ),
      ),
    );

/// The changelog's two levels: "- **Change**" and "  - detail".
List<Widget> _lines(String text) => [
      for (final line in text.split('\n'))
        if (line.trim().isNotEmpty)
          Padding(
            padding: EdgeInsets.only(left: line.startsWith('  ') ? Space.lg : 0, top: line.startsWith('  ') ? 2 : Space.sm),
            child: Row(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(line.startsWith('  ') ? '·  ' : '•  ', style: AppText.body),
                Expanded(
                  child: Text(
                    line.trim().replaceFirst(RegExp(r'^- '), '').replaceAll('**', ''),
                    style: line.startsWith('  ') ? AppText.caption : AppText.bodyStrong,
                  ),
                ),
              ],
            ),
          ),
    ];
