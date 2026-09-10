use std::collections::HashSet;
use std::rc::Rc;
use std::sync::{Condvar, Mutex};
use std::thread::ThreadId;
use std::time::Duration;

pub struct Rendezvous {
    arrivals: Mutex<HashSet<ThreadId>>,
    changed: Condvar,
}

pub fn Test_Concurrency_rendezvous() -> crate::UnknownType {
    crate::Value::Func1(purust_core::Func1::Static(|_| {
        crate::Value::Class(Rc::new(Rc::new(Rendezvous {
            arrivals: Mutex::new(HashSet::new()),
            changed: Condvar::new(),
        })))
    }))
}

pub fn Test_Concurrency_arrive(meeting: Rc<Rendezvous>) -> crate::UnknownType {
    crate::Value::Func1(purust_core::Func1::Shared(Rc::new(move |_| {
        tokio::task::block_in_place(|| {
            let mut arrivals = meeting.arrivals.lock().unwrap();
            arrivals.insert(std::thread::current().id());
            meeting.changed.notify_all();
            let (arrivals, _) = meeting
                .changed
                .wait_timeout_while(arrivals, Duration::from_secs(5), |ids| ids.len() < 2)
                .unwrap();
            assert_eq!(
                arrivals.len(),
                2,
                "Aff resumptions must use two worker threads"
            );
            crate::Value::Unit
        })
    })))
}

pub fn Test_Concurrency_enqueue(callback: crate::UnknownType) -> crate::UnknownType {
    crate::Value::Func1(purust_core::Func1::Shared(Rc::new(move |_| {
        let callback = callback.clone();
        tokio::spawn(async move {
            tokio::task::yield_now().await;
            callback.unwrap_func1()(crate::Value::Unit);
        });
        crate::Value::Unit
    })))
}

pub fn Test_Concurrency_threadCount(meeting: Rc<Rendezvous>) -> crate::UnknownType {
    crate::Value::Func1(purust_core::Func1::Shared(Rc::new(move |_| {
        crate::Value::Int(meeting.arrivals.lock().unwrap().len() as i64)
    })))
}
