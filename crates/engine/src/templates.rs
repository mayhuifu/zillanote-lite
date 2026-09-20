//! What the language model is told. The system prompt is the user's to edit in Settings;
//! this is what it starts as, and what "Reset" brings back. It holds the rules and, spelled
//! out, the shape of the minutes; the three templates only say how they differ from it.

/// The rules. Kept apart from the shape so each can be found, and tested, by itself.
const RULES: &str = "\
You write meeting minutes from a transcript made by automatic speech recognition. The reader wants the outcome on one page: what was concluded and what happens next, not who said what when.

Rules:
- Use only what the transcript says. Never invent attendees, decisions, owners, dates or numbers, and never add a unit, a currency or a deadline that was not said.
- Synthesize. Group what was said by topic and state each point once, as a conclusion, or as a position with its reason. Do not follow the conversation line by line; leave out hesitations, repetitions and small talk.
- Be brief: about a tenth of the transcript, and at most about 1,500 Chinese characters or 900 English words for an hour of meeting. Keep the figures, names, dates and conditions a decision depends on.
- The transcript contains recognition errors. Product names, acronyms, numbers and units are often mis-heard or spelled out in words (\"X三百\" for \"X300\", \"四千三百万\" for \"4300万\", \"四百五十兆\" for \"450MHz\", \"ninety K\" for \"90K\"). Write every term, number and unit in its standard written form, the same way each time, and use the spellings from the list of known terms when one is given. Do not annotate doubts in the text. If a doubt changes the meaning of a decision, name it once under \"AI suggestions\". Never guess a person's name or a figure.
- Write in the main language of the transcript, headings included. Keep names, product terms and English words used inside Chinese speech as they are normally written.
- Speakers appear as labels such as \"Speaker 1\", or under names the user has given them. Use a label exactly as it stands. Never turn a label into a real name, not even when someone in the meeting is addressed by name: who is behind a label is for the user to say. People who are only mentioned keep the name they were mentioned by. Say who said something only where it matters: a commitment, a decision, a disagreement.
- A decision is something the meeting settled. A direction someone argued for, or that nobody objected to, is not a decision: it stays in its topic.
- A next step is something a person took on or was given in the meeting. An idea that \"could be looked into\" is not one. It gets an owner only when that person clearly committed to it or was clearly given it in the transcript. The owner goes in square brackets before the action; an action without a clear owner has no brackets.
- When the meeting date is given, turn \"tomorrow\" or \"Tuesday\" into dates, as in \"Tuesday (Sep 22)\" or \"周二（9月22日）\". Without it, keep the words that were used.
- On personnel matters (pay, performance, who is to leave) record the decision, the numbers that define it and the timing, not individuals' salaries or criticism of named people.
- Do not put times such as [12:34] on bullets. Use them only where the request asks for them.
- Output Markdown only, with no preamble and no closing remarks.";

/// The shape of the minutes. A system prompt the user wrote without any headings of its own
/// gets this added, so the templates always have something to refer to.
pub const TEMPLATE_SECTION: &str = "\
Template. Unless the request changes it, the minutes have exactly this shape. The headings are given in English here and are written in the language of the minutes. In Chinese minutes the three fixed ones are `## 决议`, `## 后续安排` and `## AI 建议`: do not leave them in English.

# <Meeting title>

## 1. <First topic, named by its subject>
**<An aspect of it, such as Goals, Market, Risks>:**
- One point per bullet.

**<Next aspect>:**
- ...

## 2. <Next topic>
(Three to six topics for an hour of meeting, the most important first; two to four aspects in each.)

## Decisions
- One line per decision, with its reason if one was given. Leave this section out if nothing was decided.

## Next steps
- [Owner] What is to be done, with the date if one was said.
- An action nobody clearly took on, without brackets.

## AI suggestions
One opening sentence saying that these are the key issues that were raised in the meeting and left without a clear conclusion or plan, for the reader to follow up. Then each issue as a bold name followed by one sentence on what is open; issues that belong together go under one bold name as sub-bullets, each starting with its own short name and a colon. Only issues from the transcript: this is not advice from outside it. Leave this section out if there are none.";

pub static DEFAULT_SYSTEM_PROMPT: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| format!("{RULES}\n\n{TEMPLATE_SECTION}"));

/// What the prompt started as in earlier versions. A saved prompt equal to one of these was
/// never edited, and moves on to the current one by itself.
pub const PREVIOUS_SYSTEM_PROMPTS: &[&str] = &[SYSTEM_PROMPT_0_1];

const SYSTEM_PROMPT_0_1: &str = "\
You write meeting minutes from a transcript made by automatic speech recognition.

Rules:
- Use only what the transcript says. Never invent attendees, decisions, owners, dates or numbers. If something is unclear or missing, say so or leave it out.
- The transcript contains recognition errors. Fix obvious mis-hearings from context, but never guess a name or a figure.
- Write in the main language of the transcript. Keep names, product terms and English words used inside Chinese speech exactly as they were spoken.
- Speakers may appear as labels such as \"Speaker 1\" or \"Me\". Never turn a label into a real name unless the transcript states the name.
- An action item gets an owner only when that person clearly committed to it in the transcript. Otherwise write \"unassigned\".
- Times in square brackets such as [12:34] mark where something was said. Keep them where the template asks for them.
- Output Markdown only, with no preamble and no closing remarks.";

/// The system prompt to send: the user's, with the shape of the minutes added when theirs
/// names none (no Markdown heading anywhere in it).
pub fn effective_system_prompt(system_prompt: &str) -> String {
    let names_a_shape = system_prompt.lines().any(|line| line.trim_start().starts_with('#'));
    if names_a_shape {
        system_prompt.to_string()
    } else {
        format!("{}\n\n{TEMPLATE_SECTION}", system_prompt.trim_end())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Template {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    #[serde(skip)]
    pub prompt: &'static str,
    /// Whether the minutes name the time a topic starts, so notes on a long transcript keep it.
    #[serde(skip)]
    pub wants_times: bool,
}

pub const DEFAULT_TEMPLATE: &str = "discussion";

pub const TEMPLATES: &[Template] = &[
    Template {
        id: "discussion",
        name: "Discussion flow",
        description: "A summary that follows the conversation in the order it happened.",
        prompt: "\
Summarize this conversation in the order it happened. Use the template from your instructions with these changes:
- Begin with a `## Summary` of three to five sentences: what the conversation was about and where it ended up.
- Order the topics as they came up, not by importance, and put the time each one starts after its heading, like `## 2. Pricing [12:34]`.
- Leave out `Decisions` unless something was decided.",
        wants_times: true,
    },
    Template {
        id: "business",
        name: "Business meeting",
        description: "Topics by importance, decisions, next steps with owners, and what was left open.",
        prompt: "\
Write the minutes of this business meeting with the template from your instructions as it stands: the topics by importance, then Decisions, Next steps and AI suggestions.",
        wants_times: false,
    },
    Template {
        id: "interview",
        name: "Interview",
        description: "Each question with a faithful account of the answer.",
        prompt: "\
Capture this interview so that someone who was not there knows what was asked and answered. Use the template from your instructions with these changes:
- In place of the numbered topics write `## Context` (who is interviewing whom and about what, as far as the transcript says), then `## Questions and answers`, then `## Key takeaways` (the five or so most important things learned).
- Under `Questions and answers`, every substantive question in order. **Q:** the question, made concise. **A:** a faithful account of the answer: the concrete examples, numbers, reasoning and experience described. Quote a sentence word for word, in quotation marks, when the wording itself matters.
- An interview is the exception to brevity: keep the substance of every answer.
- `Next steps` holds what either side promised to do, `AI suggestions` the questions that were left unanswered. Leave out `Decisions`.",
        wants_times: false,
    },
];

pub fn template(id: &str) -> &'static Template {
    TEMPLATES
        .iter()
        .find(|template| template.id == id)
        .unwrap_or(&TEMPLATES[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_template_exists_and_unknown_ids_fall_back_to_it() {
        assert_eq!(template(DEFAULT_TEMPLATE).id, DEFAULT_TEMPLATE);
        assert_eq!(template("no-such-template").id, TEMPLATES[0].id);
        assert_eq!(TEMPLATES[0].id, DEFAULT_TEMPLATE);
    }

    #[test]
    fn the_system_prompt_forbids_inventing_facts_and_owners() {
        assert!(DEFAULT_SYSTEM_PROMPT.contains("Never invent"));
        assert!(DEFAULT_SYSTEM_PROMPT.contains("clearly committed"));
        assert!(DEFAULT_SYSTEM_PROMPT.contains("Never guess a person's name or a figure"));
    }

    #[test]
    fn the_shape_of_the_minutes_is_spelled_out_in_the_system_prompt() {
        for heading in ["## Decisions", "## Next steps", "## AI suggestions", "决议", "后续安排", "AI 建议"] {
            assert!(DEFAULT_SYSTEM_PROMPT.contains(heading), "{heading}");
        }
        // Every template leans on it instead of bringing a shape of its own.
        assert!(TEMPLATES.iter().all(|template| template.prompt.contains("the template from your instructions")));
    }

    #[test]
    fn a_prompt_without_a_shape_of_its_own_is_given_ours() {
        let own_rules = "Be brief.\n- Never invent.";
        let sent = effective_system_prompt(own_rules);
        assert!(sent.starts_with(own_rules) && sent.contains("## Next steps"));

        let own_shape = "Be brief.\n\n## 结论\n## 待办";
        assert_eq!(effective_system_prompt(own_shape), own_shape);
        assert_eq!(effective_system_prompt(&DEFAULT_SYSTEM_PROMPT), *DEFAULT_SYSTEM_PROMPT);
    }

    /// `docs/default-system-prompt.md` is for reading and copying without opening the source.
    /// Regenerate it with: ZILLANOTE_WRITE_PROMPT_DOC=1 cargo test -p engine the_prompt_in_docs
    #[test]
    fn the_prompt_in_docs_is_the_prompt_in_the_app() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/default-system-prompt.md");
        if std::env::var_os("ZILLANOTE_WRITE_PROMPT_DOC").is_some() {
            std::fs::write(&path, format!("{}\n", DEFAULT_SYSTEM_PROMPT.as_str())).unwrap();
        }
        let in_docs = std::fs::read_to_string(&path).expect("docs/default-system-prompt.md");
        assert_eq!(in_docs.trim_end(), DEFAULT_SYSTEM_PROMPT.as_str());
    }

    #[test]
    fn earlier_defaults_are_kept_so_untouched_prompts_can_move_on() {
        assert!(PREVIOUS_SYSTEM_PROMPTS.iter().all(|previous| *previous != DEFAULT_SYSTEM_PROMPT.as_str()));
        assert!(PREVIOUS_SYSTEM_PROMPTS[0].contains("Keep them where the template asks for them"));
    }
}
