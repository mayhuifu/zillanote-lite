//! Conservative identity matching for named voiceprints.

pub const MIN_UNIQUE_SCORE: f32 = 0.62;
pub const MIN_UNIQUE_MARGIN: f32 = 0.08;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VoiceprintSpeakerKey {
    pub channel: i64,
    pub speaker_index: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VoiceprintAssignment {
    pub speaker: VoiceprintSpeakerKey,
    pub human_id: String,
    pub score: f32,
}

pub fn cosine_similarity(left: &[f32], right: &[f32]) -> Option<f32> {
    if left.len() != right.len() || left.is_empty() {
        return None;
    }

    let mut dot = 0.0_f32;
    let mut left_norm = 0.0_f32;
    let mut right_norm = 0.0_f32;
    for (left_value, right_value) in left.iter().zip(right) {
        dot += left_value * right_value;
        left_norm += left_value * left_value;
        right_norm += right_value * right_value;
    }

    let denom = left_norm.sqrt() * right_norm.sqrt();
    if denom == 0.0 {
        return None;
    }

    Some((dot / denom).clamp(-1.0, 1.0))
}

pub fn pick_unique_voiceprint_assignments(
    scores: &[(VoiceprintSpeakerKey, &str, f32)],
    min_score: f32,
    min_margin: f32,
) -> Vec<VoiceprintAssignment> {
    let mut best_human_by_speaker: std::collections::HashMap<
        VoiceprintSpeakerKey,
        (String, f32, f32),
    > = std::collections::HashMap::new();
    let mut best_speaker_by_human: std::collections::HashMap<
        &str,
        (VoiceprintSpeakerKey, f32, f32),
    > = std::collections::HashMap::new();

    for (speaker, human_id, score) in scores {
        update_best_pair(
            best_human_by_speaker
                .entry(*speaker)
                .or_insert_with(|| (human_id.to_string(), f32::NEG_INFINITY, f32::NEG_INFINITY)),
            *human_id,
            *score,
        );
        update_best_speaker(
            best_speaker_by_human.entry(*human_id).or_insert((
                *speaker,
                f32::NEG_INFINITY,
                f32::NEG_INFINITY,
            )),
            *speaker,
            *score,
        );
    }

    let mut assignments = Vec::new();
    for (speaker, (human_id, best, second)) in best_human_by_speaker {
        if !is_unique_best(best, second, min_score, min_margin) {
            continue;
        }
        let Some((best_speaker, human_best, human_second)) =
            best_speaker_by_human.get(human_id.as_str())
        else {
            continue;
        };
        if *best_speaker != speaker
            || !is_unique_best(*human_best, *human_second, min_score, min_margin)
        {
            continue;
        }

        assignments.push(VoiceprintAssignment {
            speaker,
            human_id,
            score: best,
        });
    }

    assignments.sort_by(|left, right| {
        left.speaker
            .channel
            .cmp(&right.speaker.channel)
            .then(left.speaker.speaker_index.cmp(&right.speaker.speaker_index))
            .then(left.human_id.cmp(&right.human_id))
    });
    assignments
}

fn update_best_pair(entry: &mut (String, f32, f32), human_id: &str, score: f32) {
    if score > entry.1 {
        if entry.0 != human_id {
            entry.2 = entry.1;
        }
        entry.0 = human_id.to_string();
        entry.1 = score;
    } else if entry.0 != human_id && score > entry.2 {
        entry.2 = score;
    }
}

fn update_best_speaker(
    entry: &mut (VoiceprintSpeakerKey, f32, f32),
    speaker: VoiceprintSpeakerKey,
    score: f32,
) {
    if score > entry.1 {
        if entry.0 != speaker {
            entry.2 = entry.1;
        }
        entry.0 = speaker;
        entry.1 = score;
    } else if entry.0 != speaker && score > entry.2 {
        entry.2 = score;
    }
}

fn is_unique_best(best: f32, second: f32, min_score: f32, min_margin: f32) -> bool {
    best >= min_score && (second.is_infinite() || best - second >= min_margin)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn speaker(channel: i64, index: i64) -> VoiceprintSpeakerKey {
        VoiceprintSpeakerKey {
            channel,
            speaker_index: Some(index),
        }
    }

    #[test]
    fn cosine_rejects_mismatched_or_empty_vectors() {
        assert_eq!(cosine_similarity(&[1.0], &[1.0, 0.0]), None);
        assert_eq!(cosine_similarity(&[], &[]), None);
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 0.0]), None);
    }

    #[test]
    fn cosine_is_one_for_identical_vectors() {
        assert_eq!(cosine_similarity(&[1.0, 0.0], &[1.0, 0.0]), Some(1.0));
    }

    #[test]
    fn assigns_a_mutual_unique_match() {
        let marco = speaker(1, 0);
        let ada = speaker(1, 1);
        let assignments = pick_unique_voiceprint_assignments(
            &[
                (marco, "marco", 0.84),
                (marco, "ada", 0.41),
                (ada, "ada", 0.81),
                (ada, "marco", 0.38),
            ],
            MIN_UNIQUE_SCORE,
            MIN_UNIQUE_MARGIN,
        );

        assert_eq!(
            assignments
                .iter()
                .map(|assignment| (assignment.speaker, assignment.human_id.as_str()))
                .collect::<Vec<_>>(),
            vec![(marco, "marco"), (ada, "ada")]
        );
    }

    #[test]
    fn does_not_guess_when_two_speakers_want_the_same_person() {
        let first = speaker(1, 0);
        let second = speaker(1, 1);
        assert!(
            pick_unique_voiceprint_assignments(
                &[(first, "marco", 0.88), (second, "marco", 0.86)],
                MIN_UNIQUE_SCORE,
                MIN_UNIQUE_MARGIN,
            )
            .is_empty()
        );
    }

    #[test]
    fn does_not_guess_when_the_margin_is_thin() {
        let remote = speaker(1, 0);
        assert!(
            pick_unique_voiceprint_assignments(
                &[(remote, "marco", 0.71), (remote, "ada", 0.68)],
                MIN_UNIQUE_SCORE,
                MIN_UNIQUE_MARGIN,
            )
            .is_empty()
        );
    }

    #[test]
    fn does_not_guess_below_the_score_floor() {
        let remote = speaker(1, 0);
        assert!(
            pick_unique_voiceprint_assignments(
                &[(remote, "marco", 0.51)],
                MIN_UNIQUE_SCORE,
                MIN_UNIQUE_MARGIN,
            )
            .is_empty()
        );
    }
}
