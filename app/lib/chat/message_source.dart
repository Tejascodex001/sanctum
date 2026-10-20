import '../src/rust/api/temp_chat.dart';

class ChatMessage {
  const ChatMessage({
    required this.body,
    required this.fromMe,
    required this.sentAt,
    this.expiresAt,
  });

  final String body;
  final bool fromMe;
  final DateTime sentAt;
  final DateTime? expiresAt;
}

abstract interface class MessageSource {
  Future<List<ChatMessage>> load();
  Future<void> send(String body);
}

/// Notes to self, kept in memory until encrypted storage lands.
class MemorySource implements MessageSource {
  final _messages = <ChatMessage>[];

  @override
  Future<List<ChatMessage>> load() async => List.of(_messages);

  @override
  Future<void> send(String body) async => _messages
      .add(ChatMessage(body: body, fromMe: true, sentAt: DateTime.now()));
}

/// A temp chat held in Rust memory; messages burn after 24 hours.
class TempSource implements MessageSource {
  TempSource(this.id);

  final int id;

  @override
  Future<List<ChatMessage>> load() async {
    DateTime t(int s) => DateTime.fromMillisecondsSinceEpoch(s * 1000);
    return [
      for (final m in await tempMessages(id: id))
        ChatMessage(
          body: m.body,
          fromMe: m.fromMe,
          sentAt: t(m.sentAt),
          expiresAt: t(m.expiresAt),
        ),
    ];
  }

  @override
  Future<void> send(String body) => tempSend(id: id, body: body);
}
