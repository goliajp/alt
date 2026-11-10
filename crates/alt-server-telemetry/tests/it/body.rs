use alt_server_telemetry::{Id, Method, Metrics, ResponseObservation, Route, Trace};
use bytes::Bytes;
use http_body::{Body, Frame};
use std::{
    collections::VecDeque,
    pin::Pin,
    task::{Context, Poll, Waker},
};
struct Stream {
    frames: VecDeque<Result<Frame<Bytes>, &'static str>>,
    end_known: bool,
}
impl Body for Stream {
    type Data = Bytes;
    type Error = &'static str;
    fn poll_frame(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        Poll::Ready(self.get_mut().frames.pop_front())
    }
    fn is_end_stream(&self) -> bool {
        self.end_known && self.frames.is_empty()
    }
}
#[test]
fn streaming_body_observes_yielded_bytes_and_exactly_one_terminal_state() {
    super::capture();
    for case in [
        "eof",
        "end_known",
        "drop",
        "error",
        "before_headers",
        "empty",
    ] {
        let metrics = Metrics::new(None).unwrap();
        let trace = Trace::start(
            metrics.clone(),
            Id::new("instance").unwrap(),
            Route::GitUpload,
            Method::Post,
        );
        let observation = ResponseObservation::new(trace);
        if case == "before_headers" {
            drop(observation);
        } else {
            let mut frames = VecDeque::new();
            if case != "empty" {
                frames.push_back(Ok(Frame::data(Bytes::from_static(b"original"))));
            }
            if case == "error" {
                frames.push_back(Err("private backend detail must not be recorded"));
            }
            let body = Stream {
                frames,
                end_known: matches!(case, "empty" | "end_known"),
            };
            let mut observed = observation.body(body, 200);
            let mut cx = Context::from_waker(Waker::noop());
            if case != "empty" {
                assert!(matches!(
                    Pin::new(&mut observed).poll_frame(&mut cx),
                    Poll::Ready(Some(Ok(_)))
                ));
            }
            if case == "eof" {
                assert!(matches!(
                    Pin::new(&mut observed).poll_frame(&mut cx),
                    Poll::Ready(None)
                ));
            }
            if case == "error" {
                assert!(matches!(
                    Pin::new(&mut observed).poll_frame(&mut cx),
                    Poll::Ready(Some(Err(_)))
                ));
            }
            drop(observed);
        }
        let text = String::from_utf8(metrics.encode().unwrap()).unwrap();
        let outcome = match case {
            "drop" | "before_headers" => "dropped",
            "error" => "failed",
            _ => "complete",
        };
        let lines: Vec<_> = text
            .lines()
            .filter(|l| l.starts_with("alt_http_responses_total{"))
            .collect();
        assert_eq!(lines.len(), 1, "{case}: {text}");
        assert!(
            lines[0].contains(&format!("transport=\"{outcome}\"}} 1")),
            "{case}: {text}"
        );
        let bytes = if matches!(case, "empty" | "before_headers") {
            0
        } else {
            8
        };
        assert!(
            text.contains(&format!(
                "alt_http_response_bytes_total{{route=\"git_upload\"}} {bytes}"
            )),
            "{case}: {text}"
        );
        assert!(text.contains("alt_http_inflight 0"));
        assert!(!text.contains("private backend"));
    }
}
