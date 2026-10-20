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
    this.friend = false,
    this.verified = false,
    this.friendChatId,
    this.hidden = false,
    this.group = false,
    this.active = true,
    this.note,
    DateTime? updatedAt,
  }) : updatedAt = updatedAt ?? DateTime.now();

  final int id;
  final String title;
  final SigilSpec sigil;
  final MessageSource source;
  final bool temp;

  /// With a friend (messages travel over Tor and get delivery ticks).
  final bool friend;

  /// Friends' chats: their key is confirmed. Can change in the chat.
  bool verified;

  /// The friend's chat (itself, or the one a temp chat is with): whether
  /// they're online is looked up by it.
  final int? friendChatId;

  /// Hidden from the list; shown only after revealing hidden chats.
  final bool hidden;

  /// A group chat (several friends, no server).
  final bool group;

  /// Groups: false once we've left or been removed.
  final bool active;

  /// Shown under the title inside the chat.
  final String? note;

  bool pinned = false;
  String? preview;
  DateTime updatedAt;

  /// Temp chats: when the oldest remaining message burns.
  DateTime? nextBurn;

  /// Messages loaded just before opening the chat, so the chat screen can
  /// show them on its very first frame.
  List<ChatMessage>? preloaded;

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
