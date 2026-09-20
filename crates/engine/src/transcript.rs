#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub text: String,
    /// The name shown in front of the line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
    /// Which of the meeting's speakers that is, so the name can change later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker_index: Option<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Transcript {
    pub duration_seconds: f64,
    pub engine: String,
    pub segments: Vec<Segment>,
}

impl Transcript {
    /// One line per segment: `[mm:ss] Speaker: text`. The same form is shown to the user
    /// and given to the language model, so the times in the minutes can be looked up.
    pub fn to_text(&self) -> String {
        self.segments
            .iter()
            .map(|segment| match &segment.speaker {
                Some(speaker) => format!("[{}] {speaker}: {}", clock(segment.start), segment.text),
                None => format!("[{}] {}", clock(segment.start), segment.text),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn is_empty(&self) -> bool {
        self.segments.iter().all(|segment| segment.text.trim().is_empty())
    }
}

pub fn clock(seconds: f64) -> String {
    let total = seconds.max(0.0) as u64;
    match total / 3600 {
        0 => format!("{:02}:{:02}", total / 60, total % 60),
        hours => format!("{hours}:{:02}:{:02}", (total % 3600) / 60, total % 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_carry_the_time_and_the_speaker_when_known() {
        let transcript = Transcript {
            duration_seconds: 4000.0,
            engine: "test".to_string(),
            segments: vec![
                Segment {
                    start: 75.4,
                    end: 80.0,
                    text: "我们开始。".to_string(),
                    speaker: None,
                    speaker_index: None,
                },
                Segment {
                    start: 3725.0,
                    end: 3730.0,
                    text: "Sounds good.".to_string(),
                    speaker: Some("Speaker 2".to_string()),
                    speaker_index: Some(1),
                },
            ],
        };

        assert_eq!(
            transcript.to_text(),
            "[01:15] 我们开始。\n[1:02:05] Speaker 2: Sounds good."
        );
    }
}
