//! Polls, date polls and plans. See `sanctum_core::poll`.

use super::session;
use sanctum_core::protocol::{Plan, Poll};
use sanctum_core::store::Vote;

pub struct PollView {
    pub question: String,
    pub options: Vec<PollOptionView>,
    /// More than one choice allowed.
    pub multi: bool,
    /// Choices are times (`PollOptionView::date`).
    pub dates: bool,
    /// A plan: when and where (its choices are Going, Maybe, Can't go).
    pub plan: Option<PlanView>,
    /// How many people have answered.
    pub answered: u32,
}

pub struct PollOptionView {
    pub label: String,
    /// Date polls: Unix seconds.
    pub date: Option<i64>,
    pub count: u32,
    /// Who chose it ("You" first).
    pub voters: Vec<String>,
    pub mine: bool,
}

pub struct PlanView {
    /// Unix seconds.
    pub when: i64,
    pub place: String,
}

/// Builds what the chat shows of a poll.
pub(crate) fn view(poll: Poll, votes: &[Vote], me: &[u8; 32], name_of: impl Fn(&[u8; 32]) -> String) -> PollView {
    let answered = votes.iter().filter(|v| !v.choices.is_empty()).count() as u32;
    let options = poll
        .options
        .into_iter()
        .enumerate()
        .map(|(i, label)| {
            let chose: Vec<&Vote> = votes.iter().filter(|v| v.choices.contains(&(i as u8))).collect();
            let mine = chose.iter().any(|v| &v.voter == me);
            let mut voters: Vec<String> = chose.iter().filter(|v| &v.voter != me).map(|v| name_of(&v.voter)).collect();
            voters.sort();
            if mine {
                voters.insert(0, "You".into());
            }
            PollOptionView {
                label,
                date: poll.dates.get(i).map(|d| *d as i64),
                count: chose.len() as u32,
                voters,
                mine,
            }
        })
        .collect();
    PollView {
        question: poll.question,
        options,
        multi: poll.multi,
        dates: !poll.dates.is_empty(),
        plan: poll.plan.map(|p| PlanView {
            when: p.when as i64,
            place: p.place,
        }),
        answered,
    }
}

fn send(chat_id: i64, poll: Poll) -> anyhow::Result<()> {
    for peer in session::with(|s| Ok(s.send_poll(chat_id, poll)?))? {
        session::poke(Some(peer));
    }
    Ok(())
}

/// Posts a poll. For a date poll, pass `dates` (Unix seconds) and leave
/// `options` empty.
pub fn poll_send(
    chat_id: i64,
    question: String,
    options: Vec<String>,
    dates: Vec<i64>,
    multi: bool,
) -> anyhow::Result<()> {
    send(
        chat_id,
        Poll {
            question,
            options,
            dates: dates.into_iter().map(|d| d.max(0) as u64).collect(),
            multi,
            plan: None,
        },
    )
}

/// Posts a plan: what, when (Unix seconds) and where.
pub fn plan_send(chat_id: i64, title: String, when: i64, place: String) -> anyhow::Result<()> {
    send(
        chat_id,
        Poll {
            question: title,
            plan: Some(Plan {
                when: when.max(0) as u64,
                place,
            }),
            ..Default::default()
        },
    )
}

/// Answers the poll at `seq` (no choices: takes the answer back). For a
/// plan: 0 going, 1 maybe, 2 can't go.
pub fn poll_vote(chat_id: i64, seq: i64, choices: Vec<u8>) -> anyhow::Result<()> {
    for peer in session::with(|s| Ok(s.vote(chat_id, seq, choices)?))? {
        session::poke(Some(peer));
    }
    Ok(())
}
