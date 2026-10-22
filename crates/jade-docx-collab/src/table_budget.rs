//! Bound Cartesian expansion before constructing cell stories or rectangles.
use thiserror::Error;

/// Maximum cell positions created or enumerated by one table structure op.
/// This is not a cap on existing files or on incremental document growth.
pub const MAX_TABLE_CELLS_PER_OPERATION: usize = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
#[error("table operation exceeds {MAX_TABLE_CELLS_PER_OPERATION} cell positions")]
pub struct TableBudgetExceeded;

pub(crate) fn check(rows: usize, columns: usize) -> Result<usize, TableBudgetExceeded> {
    rows.checked_mul(columns)
        .filter(|count| *count <= MAX_TABLE_CELLS_PER_OPERATION)
        .ok_or(TableBudgetExceeded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    proptest! {
        #[test]
        fn expansion_budget_matches_wide_arithmetic(rows in any::<usize>(), columns in any::<usize>()) {
            let expected = rows as u128 * columns as u128;
            let result = check(rows, columns);
            prop_assert_eq!(result.is_ok(), expected <= MAX_TABLE_CELLS_PER_OPERATION as u128);
            if let Ok(count) = result { prop_assert_eq!(count as u128, expected); }
        }
    }
    #[test]
    fn exact_zero_and_overflow_cases_do_not_allocate() {
        assert_eq!(check(100, 100), Ok(10000));
        assert_eq!(check(101, 100), Err(TableBudgetExceeded));
        assert_eq!(check(0, usize::MAX), Ok(0));
        assert_eq!(check(usize::MAX, 0), Ok(0));
        assert_eq!(check(usize::MAX, usize::MAX), Err(TableBudgetExceeded));
    }
}
