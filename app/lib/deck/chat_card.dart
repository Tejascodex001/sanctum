import '../chat/message_source.dart';
import '../src/rust/api/sigil.dart';

/// A chat as shown in the deck.
class ChatCard {
  ChatCard({
    required this.id,
    required this.title,
    required this.sigil,
    required this.source,
    this.temp = false,
  });

  final int id;
  final String title;
  final SigilSpec sigil;
  final MessageSource source;
  final bool temp;
}
