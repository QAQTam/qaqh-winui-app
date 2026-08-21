use crate::protocol::ConversationEvent;

pub(super) fn coalesce_adjacent_deltas(
    events: impl IntoIterator<Item = ConversationEvent>,
) -> Vec<ConversationEvent> {
    let mut coalesced: Vec<ConversationEvent> = Vec::new();
    for event in events {
        match event {
            ConversationEvent::RoundDelta {
                turn_id,
                round_num,
                kind,
                delta,
            } => {
                if let Some(ConversationEvent::RoundDelta {
                    turn_id: previous_turn,
                    round_num: previous_round,
                    kind: previous_kind,
                    delta: previous_delta,
                }) = coalesced.last_mut()
                    && *previous_turn == turn_id
                    && *previous_round == round_num
                    && *previous_kind == kind
                {
                    previous_delta.push_str(&delta);
                    continue;
                }
                coalesced.push(ConversationEvent::RoundDelta {
                    turn_id,
                    round_num,
                    kind,
                    delta,
                });
            }
            checkpoint @ ConversationEvent::BlockCheckpoint { .. } => {
                // A checkpoint is the complete current block value. If it
                // immediately follows same-target deltas/checkpoint in this
                // presentation frame, parsing the superseded value is wasted.
                if coalesced
                    .last()
                    .is_some_and(|previous| checkpoint_replaces(previous, &checkpoint))
                {
                    coalesced.pop();
                }
                coalesced.push(checkpoint);
            }
            status @ ConversationEvent::ProviderToolStatus { .. } => {
                // Provider status is replaceable by call_id; only its latest
                // value in an adjacent frame run needs to touch the tool card.
                if coalesced
                    .last()
                    .is_some_and(|previous| provider_status_replaces(previous, &status))
                {
                    coalesced.pop();
                }
                coalesced.push(status);
            }
            other => coalesced.push(other),
        }
    }
    coalesced
}

fn checkpoint_replaces(previous: &ConversationEvent, next: &ConversationEvent) -> bool {
    let ConversationEvent::BlockCheckpoint {
        turn_id,
        round_num,
        kind,
        ..
    } = next
    else {
        return false;
    };
    match previous {
        ConversationEvent::RoundDelta {
            turn_id: previous_turn,
            round_num: previous_round,
            kind: previous_kind,
            ..
        }
        | ConversationEvent::BlockCheckpoint {
            turn_id: previous_turn,
            round_num: previous_round,
            kind: previous_kind,
            ..
        } => previous_turn == turn_id && previous_round == round_num && previous_kind == kind,
        _ => false,
    }
}

fn provider_status_replaces(previous: &ConversationEvent, next: &ConversationEvent) -> bool {
    let ConversationEvent::ProviderToolStatus {
        turn_id,
        round_num,
        call_id,
        ..
    } = next
    else {
        return false;
    };
    matches!(
        previous,
        ConversationEvent::ProviderToolStatus {
            turn_id: previous_turn,
            round_num: previous_round,
            call_id: previous_call,
            ..
        } if previous_turn == turn_id
            && previous_round == round_num
            && previous_call == call_id
    )
}
