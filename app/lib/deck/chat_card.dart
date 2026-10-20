import '../chat/message_source.dart';
import '../src/rust/api/sigil.dart';

/// A chat as shown in the stack.
class ChatCard {
  ChatCard({
    required this.id,
    required this.title,
    required this.sigil,
    required this.source,
    this.temp = false,
    this.note,
    DateTime? updatedAt,
  }) : updatedAt = updatedAt ?? DateTime.now();

  final int id;
  final String title;
  final SigilSpec sigil;
  final MessageSource source;
  final bool temp;

  /// Shown under the title inside the chat.
  final String? note;

  bool pinned = false;
  String? preview;
  DateTime updatedAt;

  /// Temp chats: when the oldest remaining message burns.
  DateTime? nextBurn;

  /// Refreshes preview, activity time and burn time from the source.
  Future<void> refresh() async {
    // Persistent chats only need the newest message; temp chats are in
    // memory and also need the oldest, which burns first.
    final messages = await source.load(limit: temp ? null : 1);
    if (messages.isEmpty) {
      preview = null;
      nextBurn = null;
      return;
    }
    preview = messages.last.body;
    updatedAt = messages.last.sentAt;
    nextBurn = messages.first.expiresAt;
  }
}
