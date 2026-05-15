use difference::assert_diff;
/// e2e encoding/decoding tests
use uplc::{
    ast::{DeBruijn, Name, Program},
    parser,
};

fn round_trip_test(bytes: &[u8], code: &str) {
    let parsed_program = parser::program(code).unwrap();
    can_convert_between_de_bruijn_and_name(bytes, &parsed_program);

    let debruijn_program = parsed_program.try_into().unwrap();
    parsed_program_matches_decoded_bytes(bytes, &debruijn_program);
    encoded_program_matches_bytes(bytes, &debruijn_program);

    decoded_bytes_can_convert_into_code(bytes, code);
}

fn parsed_program_matches_decoded_bytes(bytes: &[u8], debruijn_program: &Program<DeBruijn>) {
    assert_eq!(debruijn_program, &Program::from_flat(bytes).unwrap());
}

fn encoded_program_matches_bytes(bytes: &[u8], debruijn_program: &Program<DeBruijn>) {
    assert_eq!(debruijn_program.to_flat().unwrap(), bytes);
}
fn can_convert_between_de_bruijn_and_name(bytes: &[u8], parsed_program: &Program<Name>) {
    let decoded_program: Program<DeBruijn> = Program::from_flat(bytes).unwrap();
    let name_program: Program<Name> = decoded_program.try_into().unwrap();
    assert_eq!(parsed_program, &name_program);
}

fn decoded_bytes_can_convert_into_code(bytes: &[u8], code: &str) {
    let decoded_program: Program<DeBruijn> = Program::from_flat(bytes).unwrap();
    let name_program: Program<Name> = decoded_program.try_into().unwrap();
    let pretty = name_program.to_pretty();

    assert_diff!(pretty.as_str(), code, "\n", 0);
}

#[test]
fn integer() {
    let bytes = include_bytes!("../test_data/basic/integer/integer.flat");
    let code = include_str!("../test_data/basic/integer/integer.uplc");

    round_trip_test(bytes, code);
}

#[test]
fn jpg() {
    let bytes = include_bytes!("../test_data/jpg/jpg.flat");
    let code = include_str!("../test_data/jpg/jpg.uplc");

    round_trip_test(bytes, code);
}

#[test]
fn fibonacci() {
    let bytes = include_bytes!("../test_data/fibonacci/fibonacci.flat");
    let code = include_str!("../test_data/fibonacci/fibonacci.uplc");

    round_trip_test(bytes, code);
}

#[test]
fn case_constr() {
    let bytes = include_bytes!("../test_data/case_constr/case_constr.flat");
    let code = include_str!("../test_data/case_constr/case_constr.uplc");

    round_trip_test(bytes, code);
}

#[test]
fn one_way_fibonacci() {
    let bytes = include_bytes!("../test_data/fibonacci/fibonacci.flat");
    // This code doesn't match the expected `i_unique` naming scheme, so it can't be round-tripped.
    // We still want to test these "unsanitary" cases because we can't control the naming pattern
    // the consumer uses. We just can't guarantee that the decoded Flat bytes will match their
    // names.
    let code = include_str!("../test_data/fibonacci/unsanitary_fibonacci.uplc");

    let parsed_program = parser::program(code).unwrap();
    let debruijn_program = parsed_program.try_into().unwrap();
    parsed_program_matches_decoded_bytes(bytes, &debruijn_program);
    encoded_program_matches_bytes(bytes, &debruijn_program);
}
