//! Unified progress callback type used by both scanner and manager code paths.

/// Progress callback fired as work advances: `(completed, total, current_id, phase)`.
///
/// - `completed` / `total`: counts within the current logical unit.
///   - For manage/equip: items processed out of the total lock/unlock/equip request.
///   - For a scan category: items processed out of the backpack count (or rolling
///     count for characters, whose total isn't knowable up front).
/// - `current_id`: identifier of the specific item being processed, or empty string
///   when the caller doesn't track per-item ids.
/// - `phase`: human-readable phase label. For scan jobs this doubles as the
///   category key — callers MUST pass one of `"characters"`, `"weapons"`,
///   `"artifacts"` so the server can route the update to the right per-category
///   `PhaseProgress` slot. For manage/equip the phase is a free-form label.
///
/// The `'a` lifetime lets callers pass closures that borrow from their scope;
/// a naked `dyn Fn + Send + Sync` alias would default to `'static` and force
/// every caller to Box into a 'static closure.
pub type ProgressFn<'a> = dyn Fn(usize, usize, &str, &str) + Send + Sync + 'a;
