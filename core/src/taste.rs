//! What Reels learns about what you like, kept on this phone only (sealed
//! in the store, never sent): weights for who posted a reel, where, its
//! hashtags, topic and language, from hearts, shares, watching to the end
//! and skipping. Reels are ranked by it, with some new things mixed in so
//! the feed doesn't narrow to one kind of video.
//!
//! Stored with postcard: add fields only following AGENTS.md's stored-format
//! rules.

use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Weights kept (the smallest go first).
const MAX_WEIGHTS: usize = 600;
/// Reels remembered as seen, and for how long they're not shown again.
const MAX_SEEN: usize = 4000;
const SEEN_FOR: u64 = 2 * 24 * 3600;
const MAX_LIKED: usize = 1000;
/// A weight's reach: one signal can't dominate the feed.
const LIMIT: f32 = 8.0;
/// Share of the feed picked for something new rather than for taste (less
/// once interests are picked).
const EXPLORE: f64 = 0.2;
const EXPLORE_WITH_INTERESTS: f64 = 0.08;
/// What matching a picked interest adds (twice at most).
const INTEREST: f32 = 3.0;

/// Something to watch reels about, picked before Reels starts. Matched
/// against a reel's hashtags, topic and words; the first four `words` are
/// also what the sites are searched for. `peertube` is the matching PeerTube
/// category, which its sites can search by.
#[derive(Debug)]
pub struct Interest {
    pub id: &'static str,
    pub label: &'static str,
    pub emoji: &'static str,
    pub words: &'static [&'static str],
    pub peertube: Option<u32>,
}

pub const INTERESTS: &[Interest] = &[
    Interest { id: "comedy", label: "Comedy", emoji: "😂", words: &["comedy", "funny", "memes", "humor", "humour", "meme", "lol", "joke", "jokes", "prank", "standup"], peertube: Some(9) },
    Interest { id: "music", label: "Music", emoji: "🎵", words: &["music", "singing", "cover", "guitar", "song", "songs", "singer", "piano", "rap", "hiphop", "dj", "beats", "concert"], peertube: Some(1) },
    Interest { id: "dance", label: "Dance", emoji: "💃", words: &["dance", "dancing", "dancer", "kpop", "choreography"], peertube: None },
    Interest { id: "food", label: "Food", emoji: "🍜", words: &["food", "cooking", "recipe", "baking", "recipes", "foodie", "chef", "cook", "streetfood", "dessert"], peertube: Some(18) },
    Interest { id: "travel", label: "Travel", emoji: "✈️", words: &["travel", "beach", "hiking", "roadtrip", "trip", "vacation", "mountains", "adventure", "city"], peertube: Some(6) },
    Interest { id: "animals", label: "Animals", emoji: "🐾", words: &["cats", "dogs", "animals", "pets", "animal", "pet", "cat", "dog", "kitten", "puppy", "wildlife", "birds", "catsofmastodon", "dogsofmastodon"], peertube: Some(16) },
    Interest { id: "sports", label: "Sports", emoji: "⚽", words: &["sports", "football", "cricket", "skateboarding", "sport", "soccer", "basketball", "skate", "surf", "surfing", "climbing"], peertube: Some(5) },
    Interest { id: "fitness", label: "Fitness", emoji: "💪", words: &["fitness", "gym", "workout", "yoga", "running", "exercise", "calisthenics"], peertube: None },
    Interest { id: "gaming", label: "Gaming", emoji: "🎮", words: &["gaming", "minecraft", "games", "gamer", "game", "videogames", "speedrun"], peertube: Some(7) },
    Interest { id: "art", label: "Art", emoji: "🎨", words: &["art", "drawing", "painting", "animation", "illustration", "artist", "sketch", "pottery"], peertube: Some(4) },
    Interest { id: "tech", label: "Tech", emoji: "💻", words: &["tech", "coding", "linux", "programming", "technology", "gadgets", "ai", "science", "robotics"], peertube: Some(15) },
    Interest { id: "nature", label: "Nature", emoji: "🌿", words: &["nature", "garden", "flowers", "sunset", "outdoors", "forest", "gardening", "ocean", "plants"], peertube: None },
    Interest { id: "cars", label: "Cars & bikes", emoji: "🚗", words: &["cars", "motorcycle", "racing", "car", "motorbike", "bikes", "vehicles", "supercar"], peertube: Some(3) },
    Interest { id: "style", label: "Style & beauty", emoji: "👗", words: &["fashion", "makeup", "outfit", "ootd", "style", "beauty", "skincare", "hair"], peertube: None },
    Interest { id: "diy", label: "DIY", emoji: "🛠️", words: &["diy", "woodworking", "3dprinting", "crafts", "howto", "maker", "repair", "build"], peertube: Some(12) },
    Interest { id: "movies", label: "Movies & TV", emoji: "🎬", words: &["movies", "anime", "film", "cinema", "movie", "films", "tv", "series"], peertube: Some(2) },
    Interest { id: "learning", label: "Learning", emoji: "📚", words: &["education", "history", "facts", "space", "learn", "learning", "tutorial", "til", "physics"], peertube: Some(13) },
];

/// How many of `chosen` a reel matches: a word of its text or topic is one
/// of an interest's words, or a hashtag contains one ("#kpopdance" is
/// dance; short words like "ai" must match a hashtag exactly). All
/// lowercase.
pub fn interest_matches(chosen: &[&Interest], words: &BTreeSet<String>, tags: &[String]) -> u8 {
    let hit = |w: &str| {
        words.contains(w) || tags.iter().any(|t| if w.chars().count() >= 4 { t.contains(w) } else { t == w })
    };
    chosen.iter().filter(|i| i.words.iter().any(|w| hit(w))).count().min(u8::MAX as usize) as u8
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Signal {
    Like,
    Unlike,
    Share,
    /// Watched this many times its length (1.0: once to the end; loops
    /// count more).
    Watched(f32),
    /// Swiped away within a couple of seconds.
    Skipped,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Taste {
    weights: BTreeMap<String, f32>,
    /// Reel keys, oldest first, with when they were shown.
    seen: VecDeque<(String, u64)>,
    liked: BTreeSet<String>,
}

/// A reel to rank.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub key: String,
    pub author: String,
    pub features: Vec<String>,
    pub likes: u32,
    pub upright: bool,
    /// Seconds since it was posted.
    pub age_s: u64,
    /// Picked interests it matches.
    pub interests: u8,
}

impl Taste {
    pub fn learn(&mut self, features: &[String], signal: Signal) {
        let delta = match signal {
            Signal::Like => 2.0,
            Signal::Unlike => -2.0,
            Signal::Share => 2.5,
            // Half watched barely counts; to the end and again counts a lot.
            Signal::Watched(times) => (times.clamp(0.0, 3.0) - 0.5) * 0.8,
            Signal::Skipped => -0.6,
        };
        // Old preferences fade a little with every new signal.
        for w in self.weights.values_mut() {
            *w *= 0.998;
        }
        // Who posted it says more than any one hashtag.
        let n = features.len().max(1) as f32;
        for f in features {
            let share = if f.starts_with("by:") { 1.0 } else { 1.0 / n.sqrt() };
            let w = self.weights.entry(f.clone()).or_default();
            *w = (*w + delta * share).clamp(-LIMIT, LIMIT);
        }
        if self.weights.len() > MAX_WEIGHTS {
            let mut by_size: Vec<(String, f32)> = self.weights.iter().map(|(k, v)| (k.clone(), v.abs())).collect();
            by_size.sort_by(|a, b| a.1.total_cmp(&b.1));
            for (k, _) in by_size.into_iter().take(self.weights.len() - MAX_WEIGHTS) {
                self.weights.remove(&k);
            }
        }
    }

    pub fn set_liked(&mut self, key: &str, liked: bool) {
        if liked {
            self.liked.insert(key.to_string());
            if self.liked.len() > MAX_LIKED {
                let first = self.liked.iter().next().cloned();
                if let Some(first) = first.filter(|f| f != key) {
                    self.liked.remove(&first);
                }
            }
        } else {
            self.liked.remove(key);
        }
    }

    pub fn liked(&self, key: &str) -> bool {
        self.liked.contains(key)
    }

    pub fn mark_seen(&mut self, key: &str, now: u64) {
        self.seen.push_back((key.to_string(), now));
        while self.seen.len() > MAX_SEEN {
            self.seen.pop_front();
        }
    }

    /// Shown in the last two days.
    pub fn seen_recently(&self, key: &str, now: u64) -> bool {
        self.seen.iter().rev().take_while(|(_, at)| at + SEEN_FOR > now).any(|(k, _)| k == key)
    }

    /// How much this reel fits the taste, before any chance.
    pub fn score(&self, c: &Candidate) -> f32 {
        let fit: f32 = c.features.iter().filter_map(|f| self.weights.get(f)).sum();
        let popular = (1.0 + c.likes as f32).ln() * 0.35;
        let upright = if c.upright { 1.2 } else { 0.0 };
        // Fresh things rank a little higher; a month old has lost 1.
        let age = (c.age_s as f32 / (30.0 * 86_400.0)).min(3.0);
        fit + popular + upright - age + INTEREST * c.interests.min(2) as f32
    }

    /// Picks `n` reels: mostly the best fits (with a little chance, so the
    /// order isn't the same every time), some picked at random to find new
    /// tastes, and never the same person twice in a row. Returns indexes
    /// into `candidates`.
    /// With `interests` picked, fewer picks are random: the feed sticks to
    /// them.
    pub fn rank(&self, candidates: &[Candidate], n: usize, interests: bool, rng: &mut impl Rng) -> Vec<usize> {
        let explore_rate = if interests { EXPLORE_WITH_INTERESTS } else { EXPLORE };
        let mut left: Vec<(usize, f32)> = candidates
            .iter()
            .enumerate()
            .map(|(i, c)| (i, self.score(c) + rng.gen_range(0.0..1.5)))
            .collect();
        left.sort_by(|a, b| b.1.total_cmp(&a.1));
        let mut out: Vec<usize> = Vec::new();
        while out.len() < n && !left.is_empty() {
            let explore = left.len() > 1 && rng.gen_bool(explore_rate);
            let last_author = out.last().map(|&i| candidates[i].author.as_str());
            let pick = if explore {
                rng.gen_range(0..left.len())
            } else {
                left.iter().position(|(i, _)| Some(candidates[*i].author.as_str()) != last_author).unwrap_or(0)
            };
            out.push(left.remove(pick).0);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    fn reel(key: &str, author: &str, tags: &[&str]) -> Candidate {
        let mut features = vec![format!("by:{author}")];
        features.extend(tags.iter().map(|t| format!("tag:{t}")));
        Candidate { key: key.into(), author: author.into(), features, likes: 10, upright: true, age_s: 3600, interests: 0 }
    }

    #[test]
    fn likes_and_watching_raise_skips_lower() {
        let mut t = Taste::default();
        let cats = reel("1", "ann", &["cats"]);
        let cars = reel("2", "bob", &["cars"]);
        let base = t.score(&cats);
        t.learn(&cats.features, Signal::Like);
        t.learn(&cats.features, Signal::Watched(2.0));
        t.learn(&cars.features, Signal::Skipped);
        assert!(t.score(&cats) > base + 2.0);
        assert!(t.score(&cars) < base);
        // Another cat video from someone else gains from the hashtag.
        assert!(t.score(&reel("3", "cy", &["cats"])) > base);
        // Watching only half barely moves anything.
        let before = t.score(&cars);
        t.learn(&cars.features, Signal::Watched(0.5));
        assert!((t.score(&cars) - before).abs() < 0.1);
    }

    #[test]
    fn ranking_prefers_taste_but_explores_and_mixes_people() {
        let mut t = Taste::default();
        let mut pool = Vec::new();
        for i in 0..20 {
            pool.push(reel(&format!("c{i}"), &format!("cat{}", i % 4), &["cats"]));
            pool.push(reel(&format!("d{i}"), &format!("dog{}", i % 4), &["dogs"]));
        }
        for _ in 0..5 {
            t.learn(&pool[0].features, Signal::Like);
        }
        let mut rng = StdRng::seed_from_u64(7);
        let picked = t.rank(&pool, 20, false, &mut rng);
        assert_eq!(picked.len(), 20);
        let cats = picked.iter().filter(|&&i| pool[i].key.starts_with('c')).count();
        assert!(cats >= 12, "mostly what's liked: {cats}");
        assert!(cats < 20, "but some new things too");
        for w in picked.windows(2) {
            let (a, b) = (&pool[w[0]], &pool[w[1]]);
            // Exploring may repeat a person, but taste picks never do.
            if a.author == b.author {
                assert!(a.key != b.key);
            }
        }
    }

    #[test]
    fn seen_reels_wait_two_days_and_liked_are_remembered() {
        let mut t = Taste::default();
        t.mark_seen("a", 1000);
        assert!(t.seen_recently("a", 1000 + 3600));
        assert!(!t.seen_recently("a", 1000 + SEEN_FOR + 1));
        assert!(!t.seen_recently("b", 1000));
        t.set_liked("a", true);
        assert!(t.liked("a"));
        t.set_liked("a", false);
        assert!(!t.liked("a"));
        // Round-trips through storage.
        let back: Taste = postcard::from_bytes(&postcard::to_stdvec(&t).unwrap()).unwrap();
        assert!(back.seen_recently("a", 2000));
    }

    #[test]
    fn picked_interests_lead_the_feed() {
        let t = Taste::default();
        let animals: Vec<&Interest> = INTERESTS.iter().filter(|i| i.id == "animals").collect();
        let words = |ws: &[&str]| ws.iter().map(|w| w.to_string()).collect::<BTreeSet<String>>();
        assert_eq!(interest_matches(&animals, &words(&["my", "cats", "sleeping"]), &[]), 1);
        assert_eq!(interest_matches(&animals, &words(&["cars"]), &[]), 0);
        let dance: Vec<&Interest> = INTERESTS.iter().filter(|i| i.id == "dance").collect();
        assert_eq!(interest_matches(&dance, &words(&[]), &["kpopdance".into()]), 1, "inside a hashtag");
        assert_eq!(interest_matches(&dance, &words(&["abundance"]), &[]), 0, "but not inside a word");
        let mut pool = Vec::new();
        for i in 0..30 {
            let mut cat = reel(&format!("c{i}"), &format!("a{i}"), &["cats"]);
            cat.interests = 1;
            pool.push(cat);
            pool.push(reel(&format!("x{i}"), &format!("b{i}"), &["cars"]));
        }
        let picked = t.rank(&pool, 20, true, &mut StdRng::seed_from_u64(3));
        let cats = picked.iter().filter(|&&i| pool[i].interests > 0).count();
        assert!(cats >= 17, "the picked interest leads: {cats}");
    }
}
