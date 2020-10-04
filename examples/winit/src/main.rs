#![allow(dead_code)]
use std::default::Default;
use std::future::Future;
use std::pin::Pin;
use std::task::Poll;

use futures::channel::mpsc;
use futures::channel::oneshot;
use futures::executor::block_on;
use futures::task::Context;
use futures::{Stream, StreamExt};

use tokio::spawn;
use tokio::task::JoinHandle;

use parking_lot::{const_rwlock, RwLock};

use delorean::{App, Return, A};

use winit::event::Event;
use winit::event_loop::EventLoop;
use winit::platform::unix::EventLoopExtUnix;
#[cfg(target_os = "windows")]
use winit::platform::windows::EventLoopExtWindows;
use winit::window::{Window, WindowBuilder};

#[derive(Default)]
struct Winit {
    commands: Option<mpsc::UnboundedSender<Msg>>,
}

enum Msg {
    Init,
    Any(usize),
    Off,
}

enum MyEvent {
    Windows(Window),
    Event(Msg),
}

static CLOSE_EVENT_LOOP: RwLock<bool> = const_rwlock(false);
struct MyEventLoop {
    join: Option<JoinHandle<()>>,
    interface: Option<mpsc::UnboundedReceiver<MyEvent>>,
}

impl MyEventLoop {
    fn new() -> MyEventLoop {
        let (tx, rx) = mpsc::unbounded();
        let join = spawn(async {
            let event_loop: EventLoop<()> = EventLoop::new_any_thread();
            let window = WindowBuilder::new().build(&event_loop).unwrap();
            tx.unbounded_send(MyEvent::Windows(window)).unwrap();
            event_loop.run(move |event, _, _control_flow| {
                if *CLOSE_EVENT_LOOP.read() {
                    // Exit thread with error
                    panic!();
                }
                let _ = tx;
                match event {
                    Event::WindowEvent { .. } => {}
                    Event::DeviceEvent { .. } => {}
                    Event::Suspended => {}
                    Event::Resumed => {}
                    Event::MainEventsCleared => {}
                    Event::RedrawRequested(_) => {}
                    Event::RedrawEventsCleared => {}
                    Event::LoopDestroyed => {}
                    _ => {}
                }
            });
        });
        MyEventLoop {
            join: Some(join),
            interface: Some(rx),
        }
    }
}

impl Stream for MyEventLoop {
    type Item = MyEvent;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(self.get_mut().interface.as_mut().unwrap()).poll_next(cx)
    }
}

impl Drop for MyEventLoop {
    fn drop(&mut self) {
        *CLOSE_EVENT_LOOP.write() = true;
        let _ = self.interface.take();
        let _ = block_on(self.join.take().unwrap());
    }
}

struct Commander {
    addr: A<Winit>,
    threads: Vec<(JoinHandle<()>, oneshot::Receiver<Msg>)>,
    interface: mpsc::UnboundedReceiver<Msg>,
}

impl Commander {
    fn new(addr: A<Winit>, interface: mpsc::UnboundedReceiver<Msg>) -> Commander {
        Commander {
            addr,
            interface,
            threads: vec![],
        }
    }
}

impl Drop for Commander {
    fn drop(&mut self) {
        for (j, _) in self.threads.drain(..) {
            let _ = block_on(j);
        }
    }
}

impl Stream for Commander {
    type Item = ();

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let Commander {
            addr,
            threads,
            interface,
        } = self.get_mut();
        match Pin::new(interface).poll_next(cx) {
            Poll::Pending => {
                let mut finished = vec![];
                for (i, (_, rx)) in threads.iter_mut().enumerate() {
                    if let Poll::Ready(msg) = Pin::new(rx).poll(cx) {
                        let msg = match msg {
                            Ok(msg) => msg,
                            Err(_) => panic!(),
                        };
                        finished.push(i);
                        addr.send(msg);
                    }
                }
                for i in finished {
                    let _ = threads.remove(i);
                }
                Poll::Pending
            }
            Poll::Ready(Some(msg)) => {
                if let Msg::Off = msg {
                    eprintln!("Commander Off by Msg");
                    return Poll::Ready(None);
                }
                let (tx, rx) = oneshot::channel();
                threads.push((spawn(worker(msg, tx)), rx));
                Poll::Ready(Some(()))
            }
            Poll::Ready(None) => {
                eprintln!("Commander Off by End Stream");
                Poll::Ready(None)
            }
        }
    }
}

// TODO:
async fn worker(msg: Msg, tx: oneshot::Sender<Msg>) {
    let msg = match msg {
        Msg::Any(n) => Msg::Any(n * 2),
        _ => Msg::Off,
    };
    let _ = tx.send(msg);
}

struct Joiner<A, B>(A, B);

impl<A, B> Future for Joiner<A, B>
where
    A: Future<Output = Option<()>> + Unpin,
    B: Future<Output = Option<()>> + Unpin,
{
    type Output = Option<()>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let Joiner(a, b) = self.get_mut();
        let a = Pin::new(a).poll(cx);
        let b = Pin::new(b).poll(cx);

        let result = match (a, b) {
            (Poll::Ready(a), Poll::Pending) => a,
            (Poll::Pending, Poll::Ready(b)) => b,
            (Poll::Ready(a), Poll::Ready(b)) => a.and(b),
            (Poll::Pending, Poll::Pending) => return Poll::Pending,
        };
        Poll::Ready(result)
    }
}

impl App for Winit {
    type BlackBox = ();
    type Output = usize;
    type Message = Msg;

    fn __hydrate(&mut self, addr: A<Self>) -> Return<Self::Output> {
        let (tx, rx) = mpsc::unbounded();
        self.commands.replace(tx);
        let mut commander = Commander::new(addr, rx);
        let mut event = MyEventLoop::new().map(move |x| match x {
            MyEvent::Windows(_) => addr.send(Msg::Off),
            MyEvent::Event(msg) => addr.send(msg),
        });
        Box::pin(async move {
            while Joiner(commander.next(), event.next()).await.is_some() {}
            0
        })
    }

    fn __dispatch(&mut self, msg: Self::Message, _addr: A<Self>) {
        match msg {
            Msg::Init => {
                eprintln!("Init");
                let _ = self.commands.as_ref().unwrap().unbounded_send(Msg::Any(1));
            }
            Msg::Any(x) => {
                if x % 2 == 0 {
                    let _ = self.commands.take();
                }
                eprintln!("Any {}", x);
            }
            Msg::Off => {
                eprintln!("Off");
                let _ = self.commands.take();
            }
        }
    }
}

#[tokio::main]
async fn main() -> usize {
    unsafe { A::run(Winit::default()) }.await
}
