pub fn strictly_prefers_expected(ranked: &[(bool, f64)]) -> bool {
    let Some((expected, score)) = ranked.first() else {
        return false;
    };
    *expected
        && score.is_finite()
        && ranked.len() > 1
        && ranked
            .iter()
            .skip(1)
            .all(|(expected, other)| !*expected && other.is_finite() && score > other)
}
