pub fn Test_ErrorReporting_nativePanic() -> crate::UnknownType {
    crate::Value::Func1(purust_core::Func1::Static(|_| {
        panic!("NATIVE_AFF_PANIC_SENTINEL")
    }))
}
