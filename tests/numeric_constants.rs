fn return_expr_after(src: &str, marker: &str) -> String {
    let block_start = src.find(marker).expect("missing impl marker");
    let block = &src[block_start..];
    let return_start = block.find("return ").expect("missing return") + "return ".len();
    let after_return = &block[return_start..];
    let return_end = after_return.find(';').expect("missing return semicolon");
    after_return[..return_end].trim().to_string()
}

fn parse_f32_return(expr: &str) -> f32 {
    expr.trim()
        .trim_start_matches("-(")
        .trim_start_matches('(')
        .trim_end_matches(')')
        .trim_end_matches(" as f32")
        .parse::<f32>()
        .expect("invalid f32 literal")
}

fn parse_f64_return(expr: &str) -> f64 {
    expr.trim()
        .trim_start_matches('-')
        .parse::<f64>()
        .expect("invalid f64 literal")
}

#[test]
fn hardcoded_float_limits_match_rust_constants() {
    let ord = include_str!("../std/ord/ord.ciel");

    let max_f32 = return_expr_after(ord, "impl max_value(meta::Type<f32> tag)");
    let min_f32 = return_expr_after(ord, "impl min_value(meta::Type<f32> tag)");
    assert_eq!(parse_f32_return(&max_f32).to_bits(), f32::MAX.to_bits());
    assert_eq!(parse_f32_return(&min_f32).to_bits(), f32::MAX.to_bits());
    assert!(min_f32.trim_start().starts_with('-'));

    let max_f64 = return_expr_after(ord, "impl max_value(meta::Type<f64> tag)");
    let min_f64 = return_expr_after(ord, "impl min_value(meta::Type<f64> tag)");
    assert_eq!(parse_f64_return(&max_f64).to_bits(), f64::MAX.to_bits());
    assert_eq!(parse_f64_return(&min_f64).to_bits(), f64::MAX.to_bits());
    assert!(min_f64.trim_start().starts_with('-'));
}
