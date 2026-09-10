pub fn Test_Lifetime_scenario() -> i64 {
    std::env::args()
        .nth(1)
        .unwrap_or_else(|| "0".to_string())
        .parse()
        .unwrap()
}

pub fn Test_Lifetime_panic() -> crate::UnknownType {
    crate::Value::Func1(purust_core::Func1::Static(|_| {
        panic!("intentional lifetime Rust panic")
    }))
}
