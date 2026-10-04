//! The voices the user has put a name to, and the speakers found in each meeting.
//!
//! A meeting keeps what its speakers sound like, so one can be named long after the
//! recording was processed. Naming is the only thing that teaches the app a voice:
//! recognizing someone in a later meeting never adds to what is stored about them, so a
//! wrong guess cannot feed on itself.
//!
//! A voice print is biometric data. Forgetting a voice takes it out of every meeting too, and
//! with recognition turned off no voice print is kept at all: a name then only labels the
//! speaker in that meeting.

use speakers::KnownSpeaker;

/// As many examples of one voice as are kept; the oldest goes first.
const MAX_EXAMPLES: usize = 5;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Voice {
    pub id: String,
    pub name: String,
    /// What the voice sounded like in each meeting it was named in.
    pub examples: Vec<Vec<f32>>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MeetingSpeaker {
    pub index: usize,
    /// The name shown in the transcript: a voice's name, or "Speaker 2".
    pub label: String,
    /// The remembered voice this speaker is, by recognition or by the user's word.
    #[serde(default)]
    pub voice_id: Option<String>,
    #[serde(default)]
    pub seconds: f64,
    /// What this speaker sounds like in this meeting. Not sent to the window.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub centroid: Vec<f32>,
}

pub fn default_label(index: usize) -> String {
    format!("Speaker {}", index + 1)
}

pub fn known_speakers(voices: &[Voice]) -> Vec<KnownSpeaker> {
    voices
        .iter()
        .flat_map(|voice| {
            voice.examples.iter().map(|example| KnownSpeaker {
                id: voice.id.clone(),
                embedding: example.clone(),
            })
        })
        .collect()
}

/// Gives `speaker` the name `name`, teaching `voices` what that person sounds like.
/// An existing voice of that name gets another example; otherwise a new voice is made.
/// A speaker that carried another name before takes its example back from that voice.
/// A speaker without a voice print (recognition was off, or the voice was forgotten) is only
/// labelled, in this meeting.
pub fn name_speaker(voices: &mut Vec<Voice>, speaker: &mut MeetingSpeaker, name: &str, new_id: impl FnOnce() -> String) {
    let name = name.trim();
    if speaker.centroid.is_empty() {
        label_speaker(voices, speaker, name);
        return;
    }
    if let Some(previous) = speaker.voice_id.take() {
        forget_example(voices, &previous, &speaker.centroid);
    }

    let at = match voices.iter().position(|voice| voice.name.to_lowercase() == name.to_lowercase()) {
        Some(at) => at,
        None => {
            voices.push(Voice {
                id: new_id(),
                name: name.to_string(),
                examples: Vec::new(),
            });
            voices.len() - 1
        }
    };
    let voice = &mut voices[at];
    if !speaker.centroid.is_empty() && !voice.examples.contains(&speaker.centroid) {
        voice.examples.push(speaker.centroid.clone());
        if voice.examples.len() > MAX_EXAMPLES {
            voice.examples.remove(0);
        }
    }
    speaker.voice_id = Some(voice.id.clone());
    speaker.label = voice.name.clone();
}

/// Takes the name off `speaker` again.
pub fn unname_speaker(voices: &mut Vec<Voice>, speaker: &mut MeetingSpeaker) {
    label_speaker(voices, speaker, "");
}

/// Names `speaker` in its own meeting only, remembering nothing about the voice; an empty
/// name gives back "Speaker 2". A voice it was taught to before takes that example back.
pub fn label_speaker(voices: &mut Vec<Voice>, speaker: &mut MeetingSpeaker, name: &str) {
    if let Some(previous) = speaker.voice_id.take() {
        forget_example(voices, &previous, &speaker.centroid);
    }
    let name = name.trim();
    speaker.label = if name.is_empty() { default_label(speaker.index) } else { name.to_string() };
}

/// Whether the speaker carries a name the user gave, rather than "Speaker 2".
pub fn is_named(speaker: &MeetingSpeaker) -> bool {
    speaker.voice_id.is_some() || speaker.label != default_label(speaker.index)
}

/// Takes a forgotten voice out of one meeting: its speakers lose their voice print and the
/// link to the voice, and keep the name the transcript already shows. Says whether anything
/// changed.
pub fn forget_in_meeting(speakers: &mut [MeetingSpeaker], voice_id: &str) -> bool {
    let mut changed = false;
    for speaker in speakers.iter_mut().filter(|speaker| speaker.voice_id.as_deref() == Some(voice_id)) {
        speaker.voice_id = None;
        speaker.centroid.clear();
        changed = true;
    }
    changed
}

fn forget_example(voices: &mut Vec<Voice>, voice_id: &str, example: &[f32]) {
    if let Some(voice) = voices.iter_mut().find(|voice| voice.id == voice_id) {
        voice.examples.retain(|kept| kept != example);
    }
    // A voice nothing is known about can never be recognized: it is only a leftover name.
    voices.retain(|voice| voice.id != voice_id || !voice.examples.is_empty());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn speaker(index: usize, centroid: &[f32]) -> MeetingSpeaker {
        MeetingSpeaker {
            index,
            label: default_label(index),
            voice_id: None,
            seconds: 60.0,
            centroid: centroid.to_vec(),
        }
    }

    fn ids() -> impl FnMut() -> String {
        let mut next = 0;
        move || {
            next += 1;
            format!("voice-{next}")
        }
    }

    #[test]
    fn naming_a_speaker_remembers_the_voice_under_that_name() {
        let mut voices = Vec::new();
        let mut hui = speaker(0, &[1.0, 0.0]);

        name_speaker(&mut voices, &mut hui, "  Hui ", ids());

        assert_eq!((hui.label.as_str(), hui.voice_id.as_deref()), ("Hui", Some("voice-1")));
        assert_eq!(voices.len(), 1);
        assert_eq!(voices[0].examples, [vec![1.0, 0.0]]);
        assert_eq!(known_speakers(&voices).len(), 1);
    }

    #[test]
    fn the_same_name_in_another_meeting_adds_an_example_and_old_ones_make_room() {
        let mut voices = Vec::new();
        for meeting in 0..7 {
            let mut hui = speaker(0, &[meeting as f32, 1.0]);
            name_speaker(&mut voices, &mut hui, if meeting % 2 == 0 { "Hui" } else { "hui" }, ids());
        }

        assert_eq!(voices.len(), 1);
        assert_eq!(voices[0].examples.len(), MAX_EXAMPLES);
        assert_eq!(voices[0].examples[0], [2.0, 1.0]);
    }

    #[test]
    fn a_corrected_name_takes_the_example_back() {
        let mut voices = Vec::new();
        let mut speaker = speaker(1, &[0.0, 1.0]);
        let mut next_id = ids();

        name_speaker(&mut voices, &mut speaker, "Alice", &mut next_id);
        name_speaker(&mut voices, &mut speaker, "Bob", &mut next_id);

        assert_eq!(voices.iter().map(|voice| voice.name.as_str()).collect::<Vec<_>>(), ["Bob"]);
        assert_eq!(speaker.label, "Bob");

        unname_speaker(&mut voices, &mut speaker);

        assert!(voices.is_empty());
        assert_eq!((speaker.label.as_str(), speaker.voice_id), ("Speaker 2", None));
    }

    #[test]
    fn a_speaker_without_a_voice_print_is_named_in_its_meeting_only() {
        let mut voices = Vec::new();
        let mut speaker = speaker(2, &[]);

        name_speaker(&mut voices, &mut speaker, " Ada ", ids());

        assert!(voices.is_empty());
        assert_eq!((speaker.label.as_str(), speaker.voice_id.as_deref()), ("Ada", None));
        assert!(is_named(&speaker));
        unname_speaker(&mut voices, &mut speaker);
        assert_eq!(speaker.label, "Speaker 3");
        assert!(!is_named(&speaker));
    }

    #[test]
    fn labelling_a_speaker_that_taught_a_voice_takes_the_example_back() {
        let mut voices = Vec::new();
        let mut speaker = speaker(0, &[1.0, 0.0]);
        name_speaker(&mut voices, &mut speaker, "Hui", ids());

        label_speaker(&mut voices, &mut speaker, "Hui");

        assert!(voices.is_empty(), "nothing is remembered about the voice any more");
        assert_eq!((speaker.label.as_str(), speaker.voice_id), ("Hui", None));
    }

    #[test]
    fn a_forgotten_voice_leaves_the_meeting_with_its_voice_print_and_keeps_the_name_shown() {
        let mut speakers = vec![speaker(0, &[1.0, 0.0]), speaker(1, &[0.0, 1.0])];
        let mut voices = Vec::new();
        name_speaker(&mut voices, &mut speakers[0], "Hui", ids());

        assert!(forget_in_meeting(&mut speakers, "voice-1"));

        assert_eq!((speakers[0].label.as_str(), speakers[0].voice_id.as_deref()), ("Hui", None));
        assert!(speakers[0].centroid.is_empty());
        assert_eq!(speakers[1].centroid, [0.0, 1.0], "another speaker's is no business of this");
        assert!(!forget_in_meeting(&mut speakers, "voice-1"));
    }

    #[test]
    fn a_voice_named_in_two_meetings_survives_one_correction() {
        let mut voices = Vec::new();
        let mut first = speaker(0, &[1.0, 0.0]);
        let mut second = speaker(0, &[0.9, 0.1]);
        let mut next_id = ids();
        name_speaker(&mut voices, &mut first, "Hui", &mut next_id);
        name_speaker(&mut voices, &mut second, "Hui", &mut next_id);

        unname_speaker(&mut voices, &mut second);

        assert_eq!(voices[0].examples, [vec![1.0, 0.0]]);
    }
}
