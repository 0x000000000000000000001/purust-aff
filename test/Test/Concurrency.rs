use std::collections::HashSet;
use std::rc::Rc;
use std::sync::{Condvar, Mutex};
use std::thread::ThreadId;
use std::time::Duration;

pub struct Rendezvous {
    arrivals: Mutex<HashSet<ThreadId>>,
    changed: Condvar,
}

pub struct CallbackGate {
    callbacks: Mutex<Vec<crate::UnknownType>>,
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
        // Release this Tokio worker while waiting, including on a one-worker
        // runtime. A serialized callback dispatcher still cannot reach a second
        // arrival before this callback returns and must fail the bounded wait.
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
                "Aff resumptions must overlap on two worker threads"
            );
            crate::Value::Unit
        })
    })))
}

pub fn Test_Concurrency_enqueue(callback: crate::UnknownType) -> crate::UnknownType {
    crate::Value::Func1(purust_core::Func1::Shared(Rc::new(move |_| {
        enqueue_callback(callback.clone());
        crate::Value::Unit
    })))
}

fn enqueue_callback(callback: crate::UnknownType) {
    tokio::spawn(async move {
        tokio::task::yield_now().await;
        callback.unwrap_func1()(crate::Value::Unit);
    });
}

pub fn Test_Concurrency_callbackGate() -> crate::UnknownType {
    crate::Value::Func1(purust_core::Func1::Static(|_| {
        crate::Value::Class(Rc::new(Rc::new(CallbackGate {
            callbacks: Mutex::new(Vec::new()),
        })))
    }))
}

pub fn Test_Concurrency_enqueueGated(
    gate: Rc<CallbackGate>,
    callback: crate::UnknownType,
) -> crate::UnknownType {
    crate::Value::Func1(purust_core::Func1::Shared(Rc::new(move |_| {
        gate.callbacks.lock().unwrap().push(callback.clone());
        crate::Value::Unit
    })))
}

pub fn Test_Concurrency_releaseCallbacks(gate: Rc<CallbackGate>) -> crate::UnknownType {
    crate::Value::Func1(purust_core::Func1::Shared(Rc::new(move |_| {
        let callbacks = std::mem::take(&mut *gate.callbacks.lock().unwrap());
        assert_eq!(callbacks.len(), 2, "Both forks must suspend before release");
        for callback in callbacks {
            enqueue_callback(callback);
        }
        crate::Value::Unit
    })))
}

pub fn Test_Concurrency_threadCount(meeting: Rc<Rendezvous>) -> crate::UnknownType {
    crate::Value::Func1(purust_core::Func1::Shared(Rc::new(move |_| {
        crate::Value::Int(meeting.arrivals.lock().unwrap().len() as i64)
    })))
}
