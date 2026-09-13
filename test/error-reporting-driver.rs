fn main() {
    use purust_core::Value;
    let args: Vec<String> = std::env::args().collect();
    let scenario = args[1].parse::<i64>().unwrap();
    let run = || {
        Purs_Effect_Aff::purust_aff_run_main(|| {
            Purs_Test_ErrorReporting::Test_ErrorReporting_runScenario(scenario).unwrap_func1()(
                Value::Unit,
            )
        });
    };
    if args.get(2).map(String::as_str) == Some("catch-outer") {
        // A failed stderr write must not replace the original exception payload.
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)).unwrap_err();
        let error = panic
            .downcast::<std::sync::Arc<Purs_Effect_Exception::PurustExceptionError>>()
            .expect("The original PureScript exception must survive reporting");
        assert_eq!(
            purust_core::purust_string_to_utf8_lossy(&error.name),
            "ErreurΩ"
        );
        assert_eq!(
            purust_core::purust_string_to_utf8_lossy(&error.message),
            "échec 🚀 漢字"
        );
        println!("original error preserved");
    } else {
        run();
    }
}
