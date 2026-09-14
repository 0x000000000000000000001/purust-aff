pub fn Test_NativeIO_start() -> crate::UnknownType {
    crate::Value::Func1(purust_core::Func1::Static(|_| {
        let scenario: u8 = std::env::args().nth(1).unwrap_or_else(|| "0".into()).parse().unwrap();
        Purs_Effect_Aff::purust_aff_spawn_native(async move {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            if scenario == 1 { panic!("intentional native IO future panic"); }
            Ok(crate::Value::Unit)
        }, move |result| {
            if scenario == 2 { panic!("intentional native IO callback panic"); }
            assert_eq!(result.is_ok(), scenario != 1);
            println!("[OK] native IO callback completed");
            // The completion callback runs inside the originating Aff scope.
            Purs_Effect_Aff::purust_aff_spawn_native(async {
                tokio::task::yield_now().await;
                Ok(crate::Value::Unit)
            }, |result| {
                assert!(result.is_ok());
                println!("[OK] nested native IO completed");
            });
        });
        crate::Value::Unit
    }))
}
