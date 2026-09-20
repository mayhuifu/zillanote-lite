//! What the language model is told. The system prompt is the user's to edit in Settings;
//! this is what it starts as, and what "Reset" brings back.

pub const DEFAULT_SYSTEM_PROMPT: &str = "\
You write meeting minutes from a transcript made by automatic speech recognition.

Rules:
- Use only what the transcript says. Never invent attendees, decisions, owners, dates or numbers. If something is unclear or missing, say so or leave it out.
- The transcript contains recognition errors. Fix obvious mis-hearings from context, but never guess a name or a figure.
- Write in the main language of the transcript. Keep names, product terms and English words used inside Chinese speech exactly as they were spoken.
- Speakers may appear as labels such as \"Speaker 1\" or \"Me\". Never turn a label into a real name unless the transcript states the name.
- An action item gets an owner only when that person clearly committed to it in the transcript. Otherwise write \"unassigned\".
- Times in square brackets such as [12:34] mark where something was said. Keep them where the template asks for them.
- Output Markdown only, with no preamble and no closing remarks.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Template {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    #[serde(skip)]
    pub prompt: &'static str,
}

pub const DEFAULT_TEMPLATE: &str = "discussion";

pub const TEMPLATES: &[Template] = &[
    Template {
        id: "discussion",
        name: "Discussion flow",
        description: "A summary that follows the conversation in the order it happened.",
        prompt: "\
Summarize this conversation in the order it happened, keeping the essential content.

## Summary
Three to five sentences: what the conversation was about and where it ended up.

## Discussion
One short section per topic, in the order the topics came up. Give each a `###` heading that names the topic, followed by the time it starts, like `### Pricing [12:34]`. Under it, the essential points: what was said, the reasoning, any disagreement, and what was concluded. Prefer substance over completeness; leave out small talk.

## Open points
Questions left unanswered or things to be settled later. Leave this section out if there are none.",
    },
    Template {
        id: "business",
        name: "Business meeting",
        description: "Decisions, action items with owners and dates, and open questions.",
        prompt: "\
Write the minutes of this business meeting.

## Overview
Two to four sentences: the purpose of the meeting and its outcome.

## Key discussion points
Bullets grouped by topic, with the facts, figures and arguments that mattered.

## Decisions
Each decision as one bullet, with the reason if one was given. Write \"None recorded\" if nothing was decided.

## Action items
A table with the columns Action, Owner and Due. List an owner only if that person clearly committed to the action; otherwise write \"unassigned\". List a due date only if one was said. Write \"None recorded\" if there are none.

## Open questions
What was raised and not resolved.

## Follow-ups
The next meeting, or material someone promised to send, if either was mentioned.",
    },
    Template {
        id: "interview",
        name: "Interview",
        description: "Each question with a faithful account of the answer.",
        prompt: "\
Capture this interview so that someone who was not there knows what was asked and answered.

## Context
Who is interviewing whom and about what, as far as the transcript says.

## Questions and answers
Every substantive question in order. For each one:
**Q:** the question, made concise. Add the time it was asked, like [05:12].
**A:** a faithful account of the answer: the concrete examples, numbers, reasoning and experience described. Quote a sentence word for word, in quotation marks, when the wording itself matters.

## Key takeaways
The five or so most important things learned, as bullets.

## Follow-ups
Questions that were left unanswered, and anything either side promised to do.",
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
    }
}
