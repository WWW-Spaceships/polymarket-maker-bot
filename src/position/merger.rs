//! YES/NO position merger — merge complementary positions via CTF to free capital.
//!
//! When holding both YES and NO shares of the same market, they can be merged
//! (redeemed) at the CTF contract to recover USDC (1 YES + 1 NO = $1 USDC).
//! This eliminates directional risk and frees capital for new trades.

use tracing::{info, warn};

use crate::position::tracker::PositionTracker;
use std::sync::Arc;

/// Check and execute merges for all positions.
pub async fn check_and_merge(
    position_tracker: &Arc<PositionTracker>,
    _paper_mode: bool,
) -> anyhow::Result<usize> {
    let positions = position_tracker.all_positions();
    let mut merges = 0;

    // Group positions by condition_id
    let mut by_condition: std::collections::HashMap<String, Vec<_>> = std::collections::HashMap::new();
    for pos in &positions {
        by_condition
            .entry(pos.condition_id.clone())
            .or_default()
            .push(pos);
    }

    for (condition_id, tokens) in &by_condition {
        if tokens.len() < 2 {
            continue;
        }

        // Find min shares across all tokens — that's how many sets we can merge
        let min_shares = tokens.iter().map(|t| t.shares).fold(f64::MAX, f64::min);

        if min_shares >= 1.0 {
            info!(
                condition_id,
                mergeable_sets = min_shares as u64,
                "positions eligible for CTF merge"
            );

            // TODO: Call CTF merge contract
            // For now, just log the opportunity
            // In production:
            // 1. Build merge transaction
            // 2. Sign with private key
            // 3. Submit to Polygon
            // 4. Wait for confirmation
            // 5. Update position tracker

            warn!(
                condition_id,
                "CTF merge not yet implemented — logging opportunity only"
            );

            merges += 1;
        }
    }

    Ok(merges)
}
