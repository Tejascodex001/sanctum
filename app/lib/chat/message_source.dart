import '../src/rust/api/store.dart';
import '../src/rust/api/temp_chat.dart';

class ChatMessage {
  const ChatMessage({
    required this.body,
    required this.fromMe,
    required this.sentAt,
    this.expiresAt,
    this.delivered = false,
  });

  final String body;
  final bool fromMe;
  final DateTime sentAt;
  final DateTime? expiresAt;

  /// Our messages: the friend's phone has it.
  final bool delivered;
}

DateTime _fromUnix(int s) => DateTime.fromMillisecondsSinceEpoch(s * 1000);

abstract interface class MessageSource {
  /// Messages oldest first; [limit] keeps only the newest.
  Future<List<ChatMessage>> load({int? limit});
  Future<void> send(String body);
}

/// A persistent chat in the encrypted store.
class StoreSource implements MessageSource {
  StoreSource(this.chatId);

  final int chatId;

  @override
  Future<List<ChatMessage>> load({int? limit}) async => [
        for (final m in await messageList(chatId: chatId, limit: limit))
          ChatMessage(body: m.body, fromMe: m.fromMe, sentAt: _fromUnix(m.sentAt), delivered: m.delivered),
      ];

  @override
  Future<void> send(String body) => messageSend(chatId: chatId, body: body);
}

/// A temp chat held in Rust memory (on both phones, never on disk);
/// messages burn after 24 hours.
class TempSource implements MessageSource {
  TempSource(this.id);

  final int id;

  @override
  Future<List<ChatMessage>> load({int? limit}) async {
    final all = [
      for (final m in await tempMessages(id: id))
        ChatMessage(
          body: m.body,
          fromMe: m.fromMe,
          sentAt: _fromUnix(m.sentAt),
          expiresAt: _fromUnix(m.expiresAt),
          delivered: m.delivered,
        ),
    ];
    return limit == null || all.length <= limit ? all : all.sublist(all.length - limit);
  }

  @override
  Future<void> send(String body) => tempSend(id: id, body: body);
}
