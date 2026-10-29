import '../src/rust/api/poll.dart';
import '../src/rust/api/store.dart';
import '../src/rust/api/temp_chat.dart';

class ChatMessage {
  const ChatMessage({
    required this.body,
    required this.fromMe,
    required this.sentAt,
    this.expiresAt,
    this.delivered = false,
    this.seq,
    this.reply,
    this.senderName,
    this.media,
    this.poll,
    this.album,
    this.reactions = const [],
    this.call,
    this.notice = false,
    this.sharedBy,
    this.edited = false,
    this.canEdit = false,
  });

  final String body;
  final bool fromMe;
  final DateTime sentAt;
  final DateTime? expiresAt;

  /// Our messages: the friend's phone has it.
  final bool delivered;

  /// Position in a persistent chat; what replies point at.
  final int? seq;

  /// The message this one answers.
  final ChatQuote? reply;

  /// In a group: who wrote it.
  final String? senderName;

  /// A photo, video, voice note or file.
  final MediaView? media;

  /// A poll, date poll or plan, with everyone's answers.
  final PollView? poll;

  /// The shared album a photo or video is in.
  final AlbumView? album;

  /// Emoji reactions, most used first.
  final List<ReactionView> reactions;

  /// A call's record (the words are in [body]); [fromMe]: we called.
  final CallLogView? call;

  /// A line about the group itself ("Sam joined and was shown the last week"),
  /// shown in the middle of the chat.
  final bool notice;

  /// Earlier group messages the leader showed a newcomer: who passed them on.
  final String? sharedBy;

  /// Its writer changed the words after sending.
  final bool edited;

  /// We can still change its words (ours, and recent enough).
  final bool canEdit;

  /// Call records and notices aren't messages: no replies, reactions or ticks.
  bool get isRecord => call != null || notice;
}

/// The part of a message a reply shows.
class ChatQuote {
  const ChatQuote({required this.author, required this.fromMe, required this.snippet, this.seq});

  final String author;
  final bool fromMe;
  final String snippet;

  /// Where the quoted message is, if it's still in the chat.
  final int? seq;
}

DateTime _fromUnix(int s) => DateTime.fromMillisecondsSinceEpoch(s * 1000);

abstract interface class MessageSource {
  /// Messages oldest first; [limit] keeps only the newest.
  Future<List<ChatMessage>> load({int? limit});

  /// Sends [body], answering the message at [replyTo] if given.
  Future<void> send(String body, {int? replyTo});

  /// Changes the words of our message at [seq] (only ones with [ChatMessage.canEdit]).
  Future<void> edit(int seq, String body);

  /// Whether messages here can be replied to.
  bool get replies;
}

/// A persistent chat in the encrypted store.
class StoreSource implements MessageSource {
  StoreSource(this.chatId);

  final int chatId;

  @override
  Future<List<ChatMessage>> load({int? limit}) async => [
        for (final m in await messageList(chatId: chatId, limit: limit))
          ChatMessage(
            body: m.body,
            fromMe: m.fromMe,
            sentAt: _fromUnix(m.sentAt),
            delivered: m.delivered,
            seq: m.seq,
            senderName: m.senderName,
            media: m.media,
            poll: m.poll,
            album: m.album,
            reactions: m.reactions,
            call: m.call,
            notice: m.notice,
            sharedBy: m.sharedBy,
            edited: m.edited,
            canEdit: m.canEdit,
            reply: m.reply == null
                ? null
                : ChatQuote(
                    author: m.reply!.author,
                    fromMe: m.reply!.fromMe,
                    snippet: m.reply!.snippet,
                    seq: m.reply!.seq,
                  ),
          ),
      ];

  @override
  Future<void> send(String body, {int? replyTo}) => messageSend(chatId: chatId, body: body, replyTo: replyTo);

  @override
  Future<void> edit(int seq, String body) => messageEdit(chatId: chatId, seq: seq, body: body);

  @override
  bool get replies => true;
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
  Future<void> send(String body, {int? replyTo}) => tempSend(id: id, body: body);

  /// Temp messages burn as they are.
  @override
  Future<void> edit(int seq, String body) async {}

  @override
  bool get replies => false;
}
