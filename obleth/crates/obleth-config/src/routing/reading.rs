//! How hard a request looks, from its words alone.
//!
//! The heuristic reading has no model to ask, so it works from evidence in the
//! text. The rule it follows: **the middle is the default, and both ends need
//! evidence.** A request is simple only when it shows it is (a greeting, a
//! thanks, a short look-up), hard only when it shows that (it asks for a
//! design, a proof, a diagnosis, or it is long and full of constraints), and
//! anything else is moderate. Reading a hard request as simple sends it to a
//! model that answers it badly; reading a simple one as moderate only costs a
//! little. So silence is never taken for simplicity.
//!
//! A short follow-up ("continue", "that didn't fix it") says nothing by
//! itself: it is as hard as the request it follows.
//!
//! Evidence is taken from the shape of a request before its vocabulary,
//! because people do not announce a hard request with the word "design":
//!
//! - a fault that comes and goes, or shows in one place and not another, is a
//!   diagnosis ("only on one partition", "different results between runs");
//! - a choice among several options with figures attached is a decision to be
//!   reasoned through;
//! - something to build that must also be thread-safe, or constant-time, or
//!   meet five requirements at once, is more than its verb says;
//! - a short instruction applied to a short text the message carries ("fix
//!   grammar: ...") is a small edit however it is worded.
//!
//! `tests/auto_eval.rs` scores this against prompts a person labelled.

/// What one message shows about how hard it is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Evidence {
    /// Shown to be simple: a greeting, a thanks, a short look-up.
    Simple,
    /// A real piece of work with a known shape: write, explain, summarize.
    Work,
    /// Has to be worked out: a design, a proof, a diagnosis.
    Hard,
    /// Nothing either way.
    None,
}

/// A message that only greets, thanks or acknowledges.
const PLEASANTRIES: &[&str] = &[
    "hi",
    "hello",
    "helloo",
    "hey",
    "yo",
    "thanks",
    "thank you",
    "thx",
    "ty",
    "ok",
    "okay",
    "k",
    "cool",
    "nice",
    "great",
    "perfect",
    "awesome",
    "got it",
    "makes sense",
    "bye",
    "goodbye",
    "good morning",
    "good afternoon",
    "good evening",
    "lol",
    "yes",
    "no",
    "sure",
    "that's all",
    "are you there",
];

/// How a look-up starts: it asks for a fact, a name or a command.
const LOOKUPS: &[&str] = &[
    "what is ",
    "what's ",
    "whats ",
    "what are ",
    "what does ",
    "what do ",
    "what year ",
    "what port ",
    "what can you ",
    "who ",
    "when ",
    "where ",
    "which ",
    "is ",
    "are ",
    "does ",
    "do ",
    "can ",
    "how do i ",
    "how do you ",
    "how can i ",
    "how to ",
    "how many ",
    "how much ",
    "define ",
    "spell ",
    "translate ",
    "give me a synonym",
    "and ",
];

/// Words that turn a look-up into work: the answer has to be built.
const NOT_A_LOOKUP: &[&str] = &[
    "explain",
    "compare",
    "why ",
    "walk me through",
    "step by step",
    "pros and cons",
    "difference between",
    "when would",
    "when each",
    "how does",
    "how would",
    "what could",
    "choose between",
    "show the steps",
    "show your work",
    "```",
];

/// One-line edits of a sentence the message itself carries.
const SMALL_EDITS: &[&str] = &[
    "fix the grammar",
    "make this sentence",
    "make this shorter",
    "correct the spelling",
    "what's the plural",
    "in one sentence",
];

/// A piece of work with a known shape.
const WORK: &[&str] = &[
    "write ",
    "implement",
    "create ",
    "build ",
    "draft ",
    "summarize",
    "summarise",
    "explain",
    "compare",
    "convert ",
    "calculate",
    "solve",
    "walk me through",
    "rewrite",
    "rephrase",
    "proofread",
    "generate ",
    "add ",
    "show the steps",
    "how does",
    "pros and cons",
    "difference between",
    "what's wrong",
    "what is wrong",
    "how do i fix",
    "how do i choose",
    "what does that mean",
    "when would i",
    "review ",
    "outline ",
];

/// Asks that have to be worked out. Two of these, or one in a long request,
/// make it hard.
const HARD: &[&str] = &[
    "design ",
    "architect",
    "prove ",
    "proof",
    "derive ",
    "root cause",
    "race condition",
    "deadlock",
    "lock-free",
    "trade-off",
    "tradeoff",
    "trade off",
    "optimize",
    "optimise",
    "refactor",
    "diagnose",
    "bottleneck",
    "systematic",
    "first principles",
    "consistency model",
    "critique",
    "migrate ",
    "migration",
    "no downtime",
    "roll back",
    "strategy",
    "failure mode",
    "confounder",
    "generalize",
    "justify",
    "likely causes",
    "what could make",
    "can't reproduce",
    "cannot reproduce",
    "how would you",
    "how would i",
    "what would you change",
    "what would change",
    "lay out",
    "plan a ",
    "propose ",
    "scales to",
    "why might",
    "think it through",
    "think through",
    "figure out",
    "track down",
    "what you'd ",
    "what would you ",
];

/// Asks that are hard however briefly they are put.
const ALWAYS_HARD: &[&str] = &[
    "prove",
    "proof",
    "disprove",
    "show that",
    "derive",
    "root cause",
    "race condition",
    "deadlock",
    "lock-free",
    "first principles",
];

/// A program that crashed, pasted in: reading it is the hard part.
const CRASHES: &[&str] = &[
    "stack backtrace",
    "traceback (most recent call last)",
    "panicked at",
    "segmentation fault",
];

/// How a short follow-up that carries on with what came before starts.
const CARRIES_ON: &[&str] = &[
    "continue",
    "go on",
    "keep going",
    "carry on",
    "try again",
    "again",
    "now ",
    "also ",
    "yes do that",
    "yes, do that",
    "do that",
    "but ",
    "ok but",
    "okay but",
    "no,",
    "hmm",
    "actually",
    "instead",
    "wait",
];

/// How such a follow-up ends.
const CARRIES_ON_AT_THE_END: &[&str] =
    &["continue", "go on", "keep going", "carry on", "try again"];

/// Words that point back at what was said before.
const POINTS_BACK: &[&str] = &["it", "that", "this", "them", "those", "these"];

/// A fault that comes and goes, or shows in one place and not another: the
/// mark of something to diagnose, where a steady fault is something to fix.
const COMES_AND_GOES: &[&str] = &[
    "sometimes",
    "intermittent",
    "randomly",
    "occasionally",
    "once a day",
    "once a week",
    "every few",
    "every so often",
    "now and then",
    "only on ",
    "only when",
    "only happens",
    "only under",
    "only after",
    "only for ",
    "for some ",
    "between runs",
    "under load",
    "after a while",
    "since we ",
    "since i ",
    "used to ",
    "same binary",
    "same code",
    "same config",
    "same input",
    "same data",
    "same array",
    "but not on",
    "but not when",
    "non-deterministic",
    "nondeterministic",
    "flaky",
    "don't line up",
    "do not line up",
    "no pattern",
    "fine until",
    "fine to ",
    "works on ",
    "works locally",
    "out of nowhere",
];

/// The person says they do not know where to begin.
const LOST: &[&str] = &[
    "where do i even start",
    "where do i start",
    "where to start",
    "no idea why",
    "no idea what",
    "can't figure out",
    "cannot figure out",
    "at a loss",
    "stumped",
    "makes no sense",
    "out of ideas",
    "tried everything",
];

/// A choice is being asked for.
const DECIDES: &[&str] = &[
    "is it better",
    "should i ",
    "should we ",
    "which should",
    "which one",
    "which would",
    "worth it",
    "worth the",
    " vs ",
    " vs. ",
    "versus",
    "choose",
    "decide",
    "decision",
    "you'd pick",
    "you'd do",
    "would you pick",
];

/// Things counted off as alternatives.
const COUNTED_OPTIONS: &[&str] = &[
    " offers",
    " options",
    " choices",
    " candidates",
    " alternatives",
    " approaches",
];

/// Qualities that make something harder to build than its verb says.
const DEMANDING: &[&str] = &[
    "thread safe",
    "thread-safe",
    "threadsafe",
    "concurren",
    "mutex",
    "hold the lock",
    "atomic",
    "o(1)",
    "o(n",
    "o(log",
    "p99",
    "latency",
    "distributed",
    "fault toleran",
    "fault-toleran",
    "exactly once",
    "exactly-once",
    "idempoten",
    "backpressure",
    "zero downtime",
    "no downtime",
    "high availability",
    "real time",
    "real-time",
    "constant time",
    "constant-time",
    "operator precedence",
];

/// Something has gone wrong, or is being denied.
const TROUBLE: &[&str] = &[
    " not ",
    "n't",
    "cannot",
    " down",
    "error",
    "exception",
    "traceback",
    "undefined",
];

/// How the words for it start.
const TROUBLE_STEMS: &[&str] = &[
    "fail", "error", "crash", "broke", "wrong", "issue", "problem", "bug", "slow", "stuck", "hang",
    "hung",
];

/// The message wants something done.
const REQUESTS: &[&str] = &[
    "can you",
    "could you",
    "would you",
    "please",
    "help me",
    "help with",
    "i need",
    "i want",
    "how ",
    "what ",
    "why ",
    "tell me",
    "show me",
    "give me",
    "let's",
];

/// The message asks for the name of a thing, not for the thing to be made.
const NAMES_A_THING: &[&str] = &[
    " command",
    " flag",
    " option ",
    "shortcut",
    "one-liner",
    "one liner",
    "syntax for",
    "abbreviation",
    "acronym",
    "stand for",
];

/// How a question of plain fact starts, wherever in the message it comes.
const FACTS: &[&str] = &[
    "what is the ",
    "what's the ",
    "what are the ",
    "who is ",
    "who was ",
    "who wrote ",
    "who invented ",
    "how many ",
    "how much ",
    "what year ",
    "when did ",
    "when was ",
    "where is ",
];

/// A judgment is wanted on the text the message carries, not an edit of it.
const JUDGES: &[&str] = &[
    "thoughts", "feedback", "opinion", "advice", "improve", "review", "better", "evaluate",
    "assess",
];

/// Units a figure comes in, a space after the number.
const UNITS: &[&str] = &[
    "ms", "gb", "tb", "mb", "kb", "node", "core", "gpu", "cpu", "row", "rank", "hour", "minute",
    "second", "day", "week", "month", "year", "job", "group", "user", "thread", "request", "times",
    "percent", "million", "billion", "thousand",
];

/// What such a follow-up says anywhere in it: the last answer did not work,
/// or the same problem is turned another way.
const CARRIES_ON_ANYWHERE: &[&str] = &[
    "what if",
    "what about",
    "that didn't",
    "that doesn't",
    "that did not",
    "it still",
    "still fails",
    "still hangs",
    "didn't work",
    "doesn't work",
    "not working",
    "crashes",
    "try again",
];

fn words(text: &str) -> usize {
    text.split_whitespace().count()
}

fn any(list: &[&str], text: &str) -> bool {
    list.iter().any(|k| text.contains(k))
}

/// The text without its punctuation at either end, for matching whole
/// messages such as "Thanks!".
fn bare(lower: &str) -> &str {
    lower.trim_matches(|c: char| !c.is_alphanumeric())
}

/// The message in pieces, at its punctuation and at "but".
fn clauses(lower: &str) -> impl Iterator<Item = &str> {
    lower
        .split([',', '.', ';', ':', '!', '?', '\n'])
        .flat_map(|c| c.split(" but "))
        .map(str::trim)
        .filter(|c| !c.is_empty())
}

/// Something has gone wrong in what the text describes.
fn troubled(lower: &str) -> bool {
    any(TROUBLE, lower)
        || lower
            .split(|c: char| !c.is_alphanumeric())
            .any(|word| TROUBLE_STEMS.iter().any(|stem| word.starts_with(stem)))
}

/// The text refers to something said before it.
fn points_back(lower: &str) -> bool {
    POINTS_BACK.iter().any(|w| super::has_word(lower, w))
}

/// A message that only thanks or acknowledges: one of [`PLEASANTRIES`], alone
/// or opening a line that asks for nothing ("thanks, that makes sense").
pub(super) fn is_pleasantry(lower: &str) -> bool {
    let question = lower.contains('?');
    let text = bare(lower);
    if text == "are you there" || PLEASANTRIES.contains(&text) {
        return true;
    }
    if question {
        return false;
    }
    PLEASANTRIES.iter().any(|p| {
        text.strip_prefix(p).is_some_and(|rest| {
            rest.starts_with([',', ' ', '!', '.'])
                && !carries_on(rest)
                && !any(WORK, rest)
                && !any(HARD, rest)
                && !troubled(rest)
                // A few words more, or a longer line that still wants nothing.
                && (words(rest) <= 8 || (words(rest) <= 30 && !any(REQUESTS, rest)))
        })
    })
}

/// A short follow-up that carries on with the request before it.
pub(super) fn carries_on(lower: &str) -> bool {
    let text = bare(lower);
    words(text) <= 15
        && (CARRIES_ON.iter().any(|p| text.starts_with(p))
            || CARRIES_ON_AT_THE_END.iter().any(|p| text.ends_with(p))
            || any(CARRIES_ON_ANYWHERE, text))
}

/// A short follow-up that is about the request before it: it carries the work
/// on, or it points back at it ("can you make it return None instead").
pub(super) fn follows_on(lower: &str) -> bool {
    carries_on(lower) || (words(lower) <= 15 && points_back(lower))
}

/// Figures an answer has to meet or weigh: a number with a unit, a
/// percentage or a sum of money ("1 ms", "120k", "40%", "16 cores"). A bare
/// number, a year or a version is not one.
pub(super) fn figures(lower: &str) -> usize {
    let bytes = lower.as_bytes();
    let (mut count, mut at) = (0, 0);
    while at < bytes.len() {
        if !bytes[at].is_ascii_digit() {
            at += 1;
            continue;
        }
        // Part of a name ("p99", "x^2", "v2"), not a quantity.
        let named =
            at > 0 && (bytes[at - 1].is_ascii_alphabetic() || b"^_".contains(&bytes[at - 1]));
        let money = at > 0 && bytes[at - 1] == b'$';
        let mut end = at;
        while end < bytes.len()
            && (bytes[end].is_ascii_digit()
                || (b",.".contains(&bytes[end])
                    && bytes.get(end + 1).is_some_and(u8::is_ascii_digit)))
        {
            end += 1;
        }
        at = end;
        if named {
            continue;
        }
        let after = &lower[end..];
        // A unit written against the number: "6h", "120k", "2k/s", "40%".
        let letters = after.bytes().take_while(u8::is_ascii_alphabetic).count();
        let closed = after
            .as_bytes()
            .get(letters)
            .is_none_or(|b| b.is_ascii_whitespace() || b",.;)/".contains(b));
        let attached = after.starts_with('%') || ((1..=3).contains(&letters) && closed);
        let spaced = after
            .strip_prefix(' ')
            .is_some_and(|rest| UNITS.iter().any(|unit| rest.starts_with(unit)));
        if money || attached || spaced {
            count += 1;
        }
    }
    count
}

/// How many requirements a request strings together.
fn load(lower: &str) -> usize {
    [
        ", ",
        " and ",
        " with ",
        " that ",
        " that's ",
        " while ",
        " without ",
        "; ",
    ]
    .iter()
    .map(|joint| lower.matches(joint).count())
    .sum()
}

/// "keeps failing", "keep getting": the same thing, again and again.
fn keeps_happening(lower: &str) -> bool {
    ["keeps ", "keep "].iter().any(|keep| {
        lower.match_indices(keep).any(|(at, _)| {
            lower[at + keep.len()..]
                .split_whitespace()
                .next()
                .is_some_and(|next| {
                    next.trim_end_matches(|c: char| !c.is_alphanumeric())
                        .ends_with("ing")
                        && next != "going"
                })
        })
    })
}

/// Three or more alternatives: a list closed with "or", or a count of them.
fn several_options(lower: &str) -> bool {
    lower.contains(", or ") || lower.matches(" or ").count() >= 2 || any(COUNTED_OPTIONS, lower)
}

/// A short instruction applied to a short text the message carries, after a
/// colon: "fix grammar: me and him goes to the library yesterday".
fn edits_a_given_text(ask: &str, given: &str) -> bool {
    (2..=8).contains(&words(ask))
        && (1..=40).contains(&words(given))
        && !given.contains('?')
        && !troubled(ask)
        && !any(&["error", "exception", "traceback", "undefined"], given)
        && !any(JUDGES, ask)
}

/// What one message shows about how hard it is.
pub(super) fn evidence(text: &str) -> Evidence {
    let lower = text.to_ascii_lowercase();
    let lower = lower.trim();
    if lower.is_empty() {
        return Evidence::None;
    }
    if is_pleasantry(lower) {
        return Evidence::Simple;
    }
    if any(CRASHES, lower) {
        return Evidence::Hard;
    }
    // A label in front ("python: how do i ...", "bash one-off: rename ...")
    // is not part of the ask: one word, or a few that name a language or tool.
    let colon = lower.split_once(": ").or_else(|| lower.split_once(":\n"));
    if let Some((label, rest)) = colon {
        let labels = words(label) == 1
            || (words(label) <= 3 && super::heuristic_topics(label).contains(&"coding"));
        if labels && !rest.trim().is_empty() {
            return evidence(rest);
        }
    }

    let count = words(lower);
    let figures = figures(lower);
    let code = lower.contains("```");
    let works = any(WORK, lower);
    let load = load(lower);
    // Several things asked in one message.
    let parts = lower.matches('?').count()
        + lower.matches(", and ").count()
        + lower.matches(" then ").count()
        + lower.matches("including ").count();

    // Asks that have to be worked out: by what is asked for, and by the shape
    // of what is described.
    let mut asks = HARD.iter().filter(|k| lower.contains(*k)).count()
        + COMES_AND_GOES.iter().filter(|k| lower.contains(*k)).count()
        + usize::from(keeps_happening(lower))
        + usize::from(any(LOST, lower));
    // A choice with enough riding on it: several options, figures to weigh,
    // or a long account of the situation.
    if any(DECIDES, lower) && (several_options(lower) || figures >= 2 || count > 40) {
        asks += 1;
    }
    // Points toward hard: what is asked, and how much has to be held at once.
    let mut points = asks * 2;
    if count > 60 {
        points += 1;
    }
    if count > 150 {
        points += 1;
    }
    if parts >= 2 {
        points += 1;
    }
    if figures >= 2 {
        points += 1;
    }
    // Two hard asks, or one in a request with some weight to it: more than a
    // line, several parts, a figure to meet, or code to read.
    let weighty = count > 25 || parts >= 2 || figures >= 1 || code;
    let always = count > 4 && ALWAYS_HARD.iter().any(|k| super::has_word(lower, k));
    // Something to build that must meet a demanding quality as well as its
    // other requirements, or a great many requirements at once.
    let demanding = works && ((any(DEMANDING, lower) && load >= 3) || load >= 6);
    if always || asks >= 2 || (asks == 1 && weighty) || points >= 4 || demanding {
        return Evidence::Hard;
    }

    // Nothing in it asks for work.
    let quiet = asks == 0 && !works && !code;
    let troubled = troubled(lower);
    // A short question that asks for a fact, a name or a command.
    let lookup = count <= 18
        && !code
        && asks == 0
        && LOOKUPS.iter().any(|p| lower.starts_with(p))
        && !any(NOT_A_LOOKUP, lower);
    // The name of a command or a flag, however it is asked.
    let names = count <= 16 && quiet && !troubled && any(NAMES_A_THING, lower);
    // A question of fact after a word of introduction ("sorry if this is a
    // silly question, but what is the longest river in South America?").
    let fact = count <= 30
        && quiet
        && !troubled
        && lower.matches('?').count() <= 1
        && !any(NOT_A_LOOKUP, lower)
        && clauses(lower).any(|c| FACTS.iter().any(|p| c.starts_with(p)) && !points_back(c));
    let small_edit = asks == 0
        && ((count <= 30 && any(SMALL_EDITS, lower))
            || (quiet && colon.is_some_and(|(ask, given)| edits_a_given_text(ask, given))));
    if lookup || names || fact || small_edit {
        return Evidence::Simple;
    }
    if works || asks > 0 || code || count > 60 {
        return Evidence::Work;
    }
    // A few words that ask nothing much.
    if count <= 4 && !troubled && !lower.contains('?') {
        return Evidence::Simple;
    }
    Evidence::None
}

/// The difficulty, 1 to 3, of the newest of `turns` (the person's messages,
/// oldest first). The middle is the default; a short follow-up is as hard as
/// what it follows.
pub(super) fn difficulty(turns: &[String]) -> u8 {
    let Some((newest, earlier)) = turns.split_last() else {
        return 2;
    };
    let lower = newest.to_ascii_lowercase();
    let own = evidence(newest);
    // A thanks is simple wherever it comes.
    if is_pleasantry(lower.trim()) {
        return 1;
    }
    let level = |e: Evidence| match e {
        Evidence::Simple => 1,
        Evidence::Work | Evidence::None => 2,
        Evidence::Hard => 3,
    };
    // A short follow-up that carries the work on ("continue", "that didn't fix
    // it", "now make it...") is as hard as the request it follows. A new
    // question in the same conversation is read for itself.
    if !earlier.is_empty() && own != Evidence::Hard && follows_on(lower.trim()) {
        return difficulty(earlier).max(if own == Evidence::Work { 2 } else { 1 });
    }
    level(own)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(turns: &[&str]) -> u8 {
        difficulty(&turns.iter().map(|t| t.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn a_greeting_a_thanks_and_a_look_up_are_simple() {
        for text in [
            "Helloo",
            "hi",
            "Thanks!",
            "ok",
            "Thank you so much, that fixed it.",
            "What is the capital of Australia?",
            "What port does SSH use by default?",
            "How do I exit vim?",
            "Define 'idempotent'.",
            "Translate 'where is the train station' into Spanish.",
            "Fix the grammar: 'me and him goes to the lab yesterday'.",
        ] {
            assert_eq!(read(&[text]), 1, "{text}");
        }
    }

    #[test]
    fn silence_is_not_simplicity() {
        // Nothing here says simple or hard: the middle.
        for text in [
            "The quarterly numbers for the three regions and what they mean for hiring next year",
            "My Slurm job is pending with reason (Resources). What does that mean and what can I change to start sooner?",
            "Write a Python function that reads a CSV file and returns the mean of each numeric column.",
            "Explain how TCP congestion control works, with an example.",
        ] {
            assert_eq!(read(&[text]), 2, "{text}");
        }
    }

    #[test]
    fn a_design_a_proof_and_a_diagnosis_are_hard() {
        for text in [
            "Design a rate limiter for a multi-tenant API gateway that is fair across tenants, survives a node restart, and adds under 1 ms at p99. Compare token bucket in Redis with a local approximation and say which you would build.",
            "Prove that the square root of 2 is irrational, then generalize the proof to the square root of any prime.",
            "thread 'main' panicked at src/x.rs:12\n  stack backtrace:\n   0: foo",
            "Our service deadlocks under load about once a day. How would you find the root cause, and what are the likely designs that avoid it?",
        ] {
            assert_eq!(read(&[text]), 3, "{text}");
        }
    }

    #[test]
    fn a_follow_up_is_as_hard_as_what_it_follows() {
        let hard = "Our service deadlocks under load about once a day. How would you find the root cause, and what are the likely designs that avoid it?";
        let work = "Write a Python function that reads a CSV file and returns the mean of each numeric column.";
        assert_eq!(read(&[hard, "continue"]), 3);
        assert_eq!(read(&[hard, "that didn't fix it, it still hangs"]), 3);
        assert_eq!(
            read(&[
                hard,
                "ok and what if the writer task is the one that panics?"
            ]),
            3
        );
        assert_eq!(read(&[work, "now make it skip empty cells"]), 2);
        assert_eq!(
            read(&[work, "try again, that crashes on the header row"]),
            2
        );
        // A thanks is simple wherever it comes.
        assert_eq!(read(&[hard, "thanks, that makes sense"]), 1);
        assert_eq!(read(&[work, "great, thank you!"]), 1);
        // An easy follow-up to an easy question stays easy.
        assert_eq!(
            read(&["What is the capital of Australia?", "and New Zealand?"]),
            1
        );
    }

    #[test]
    fn figures_are_numbers_with_units() {
        assert_eq!(figures("under 1 ms at p99 with 2k/s writes on 400 gb"), 3);
        assert_eq!(figures("offers of 120k and $85,000, estimates 40% low"), 3);
        assert_eq!(figures("version 3 of the plan from 2024, in c++17"), 0);
    }

    #[test]
    fn a_fault_that_comes_and_goes_is_a_diagnosis() {
        for text in [
            "the job is fine until about the 40th epoch and then loss goes to nan, but only when I use two GPUs",
            "our builds sometimes take 3x longer and there is no pattern to it that I can see",
            "the page loads quickly on my laptop but takes ten seconds for some customers, and only after a deploy",
        ] {
            assert_eq!(read(&[text]), 3, "{text}");
        }
        // A steady fault is something to fix.
        assert_eq!(
            read(&["my loop prints the last value five times, what's wrong?"]),
            2
        );
    }

    #[test]
    fn a_choice_among_several_options_with_figures_is_reasoned_through() {
        assert_eq!(
            read(&["should we rent three more racks at 4k a month, buy our own for 180k, or move the archive tier out? we have about 2 years of runway"]),
            3
        );
        // Two options and nothing riding on it.
        assert_eq!(read(&["should I use tabs or spaces?"]), 2);
    }

    #[test]
    fn a_build_with_demanding_qualities_is_hard() {
        assert_eq!(
            read(&["write a connection pool in Go that is thread-safe, with a max size, idle timeouts, and health checks that never block a caller"]),
            3
        );
        assert_eq!(
            read(&["write a function that adds two numbers and returns the sum"]),
            2
        );
    }

    #[test]
    fn a_short_instruction_on_a_short_text_is_a_small_edit() {
        for text in [
            "make this friendlier: send the file today",
            "reword for a child: photosynthesis turns light into food",
            "bash: how do i list hidden files",
            "sorry to bother you, quick thing: who wrote Middlemarch",
        ] {
            assert_eq!(read(&[text]), 1, "{text}");
        }
        // A judgment on the text is not an edit of it.
        assert_eq!(
            read(&["thoughts on this plan: we move the database first and the app servers after"]),
            2
        );
    }

    #[test]
    fn a_long_greeting_that_asks_nothing_is_simple() {
        assert_eq!(read(&["Hello! I hope your week is going well, I just thought I would check in before the meeting."]), 1);
        assert_eq!(read(&["hey my job crashed"]), 2);
    }

    #[test]
    fn a_follow_up_that_points_back_is_about_what_came_before() {
        let work = "Write a Python function that reads a CSV file and returns the mean of each numeric column.";
        assert_eq!(
            read(&[work, "can you make it return None for an empty file"]),
            2
        );
        let hard = "Prove that every bounded monotone sequence of real numbers converges.";
        assert_eq!(
            read(&[
                hard,
                "ok but where exactly did that use completeness? keep going"
            ]),
            3
        );
        // A new question is read for itself.
        assert_eq!(
            read(&[hard, "ty. unrelated, what's the capital of Peru"]),
            1
        );
    }
}
