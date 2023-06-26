#![forbid(unsafe_code)]

//! Provides functions to easily integrate Leptos with Actix.
//!
//! WIP

use core::panic;
use futures::Future;
use leptos::{
    create_runtime,
    leptos_server::server_fn_by_path,
    provide_context, raw_scope_and_disposer,
    server_fn::{Encoding, Payload},
    use_context, IntoView, LeptosOptions,
};
use leptos_router::Method;
use parking_lot::RwLock;
use std::sync::Arc;
use worker::{
    Headers, Request, Response, ResponseBody, Result as WorkerResult,
};

/// This struct lets you define headers and override the status of the Response from an Element or a Server Function
/// Typically contained inside of a ResponseOptions. Setting this is useful for cookies and custom responses.
#[derive(Debug, Clone, Default)]
pub struct ResponseParts {
    pub headers: Headers,
    pub status: Option<u16>,
}

impl ResponseParts {
    /// Insert a header, overwriting any previous value with the same key
    pub fn insert_header(&mut self, key: &str, value: &str) {
        self.headers.set(key, value);
    }
    /// Append a header, leaving any header with the same key intact
    pub fn append_header(&mut self, key: &str, value: &str) {
        self.headers.append(key, value);
    }
}

/// Adding this Struct to your Scope inside of a Server Fn or Elements will allow you to override details of the Response
/// like StatusCode and add Headers/Cookies. Because Elements and Server Fns are lower in the tree than the Response generation
/// code, it needs to be wrapped in an `Arc<RwLock<>>` so that it can be surfaced
#[derive(Debug, Clone, Default)]
pub struct ResponseOptions(pub Arc<RwLock<ResponseParts>>);

impl ResponseOptions {
    /// A less boilerplatey way to overwrite the contents of `ResponseOptions` with a new `ResponseParts`
    pub fn overwrite(&self, parts: ResponseParts) {
        let mut writable = self.0.write();
        *writable = parts
    }
    /// Set the status of the returned Response
    pub fn set_status(&self, status: u16) {
        let mut writeable = self.0.write();
        let res_parts = &mut *writeable;
        res_parts.status = Some(status);
    }
    /// Insert a header, overwriting any previous value with the same key
    pub fn insert_header(
        &self,
        key: &str,
        value: &str,
    ) {
        let mut writeable = self.0.write();
        let res_parts = &mut *writeable;
        res_parts.headers.set(key, value);
    }
    /// Append a header, leaving any header with the same key intact
    pub fn append_header(
        &self,
        key: &str,
        value: &str,
    ) {
        let mut writeable = self.0.write();
        let res_parts = &mut *writeable;
        res_parts.headers.append(key, value);
    }
}

/// Provides an easy way to redirect the user from within a server function. Mimicking the Remix `redirect()`,
/// it sets a StatusCode of 302 and a LOCATION header with the provided value.
/// If looking to redirect from the client, `leptos_router::use_navigate()` should be used instead
pub fn redirect(cx: leptos::Scope, path: &str) {
    if let Some(response_options) = use_context::<ResponseOptions>(cx) {
        response_options.set_status(302);
        response_options.insert_header("Location", path);
    }
}

/// A Worker [on_async](worker::Router::on_async) that listens for theoretically any requests with
/// Leptos server function arguments in the URL (`GET`) or body (`POST`),
/// runs the server function if found, and returns the resulting [Response].
///
/// This provides the [Request] to the server [Scope](leptos::Scope).
///
/// ## Provided Context Types
/// This function always provides context values including the following types:
/// - [ResponseOptions]
/// - [Request](worker::Request)
pub async fn handle_server_fns<
    'a,
    // T: Future<Output = WorkerResult<Response>> + 'a,
    D,
>(
    req: Request,
    ctx: worker::RouteContext<D>,
) -> impl Future<Output = WorkerResult<Response>> {
    handle_server_fns_with_context(req, ctx, |_cx| {})
}

/// A Worker [on_async](worker::Router::on_async) that listens for theoretically any requests with
/// Leptos server function arguments in the URL (`GET`) or body (`POST`),
/// runs the server function if found, and returns the resulting [Response].
///
/// This provides the [Request] to the server [Scope](leptos::Scope).
///
/// This can then be set up at an appropriate route in your application:
///
/// This version allows you to pass in a closure that adds additional route data to the
/// context.
///
/// ## Provided Context Types
/// This function always provides context values including the following types:
/// - [ResponseOptions]
/// - [Request](worker::Request)
pub fn handle_server_fns_with_context<
    'a,
    // T: Future<Output = WorkerResult<Response>> + 'a,
    D,
>(
    req: Request,
    ctx: worker::RouteContext<D>,
    additional_context: impl Fn(leptos::Scope) + 'static + Clone + Send,
) -> impl Future<Output = WorkerResult<Response>> {
    let url = match req.url() {
        Ok(u) => u,
        _ => {
            panic!("Failed to get URL from request"); //how to deal with errors in here?
        }
    };

    async move {
        Ok(
            if let Some(server_fn) = server_fn_by_path(
                url.path().strip_prefix('/').unwrap_or(url.path()),
            ) {
                let runtime = create_runtime();
                let (cx, disposer) = raw_scope_and_disposer(runtime);

                additional_context(cx);

                // provide_context(cx, req); //request doesn't implement clone...
                provide_context(cx, ResponseOptions::default());

                let query = url.query().unwrap_or("");
                let data = match &server_fn.encoding() {
                    Encoding::Url | Encoding::Cbor => {
                        req.clone_mut()
                            .expect("Could not mutably clone request")
                            .bytes()
                            .await
                            .unwrap_or_default() //TODO better error handling?
                    }
                    Encoding::GetJSON | Encoding::GetCBOR => {
                        Vec::from(query.as_bytes())
                    }
                };
                let res = match server_fn.call(cx, &data).await {
                    Ok(serialized) => {
                        // If ResponseOptions are set, add the headers and status to the request
                        let res_options =
                            use_context::<ResponseOptions>(cx).unwrap();

                        let res_parts = res_options.0.write();

                        // if this is Accept: application/json then send a serialized JSON response
                        let accept_header = match req.headers().get("Accept") {
                            Ok(o) => o,
                            _ => None,
                        };
                        

                        let mut res_status: u16 = 0;
                        let mut headers = Headers::new();

                        if accept_header == Some("application/json".to_string())
                            || accept_header
                                == Some("application/x-www-form-urlencoded".to_string())
                            || accept_header == Some("application/cbor".to_string())
                        {
                            res_status = 200;
                        }
                        // otherwise, it's probably a <form> submit or something: redirect back to the referrer
                        else {
                            let referer = match req.headers().get("Referer") {
                                Ok(Some(value)) => value,
                                _ => "/".to_string(),
                            };
                            res_status = 303;
                            headers.set("Location", &referer);
                        };
                        // Override StatusCode if it was set in a Resource or Element
                        if let Some(status) = res_parts.status {
                            res_status = status;
                        }

                        res_parts
                            .headers
                            .entries()
                            .map(|(k, v)| headers.append(&k, &v));

                        match serialized {
                            Payload::Binary(data) => {
                                match Response::from_body(ResponseBody::Body(
                                    data,
                                )) {
                                    Ok(r) => r
                                        .with_headers(headers)
                                        .with_status(res_status),
                                    _ => Response::empty()
                                        .unwrap()
                                        .with_status(500), //unwrap
                                }
                            }
                            Payload::Url(data) => {
                                match Response::from_body(ResponseBody::Body(
                                    data.into_bytes(),
                                )) {
                                    Ok(r) => {
                                        headers.set(
                                            "Content-Type",
                                            "application/\
                                             x-www-form-urlendcoded",
                                        );
                                        r.with_headers(headers)
                                            .with_status(res_status)
                                    }
                                    _ => Response::empty()
                                        .unwrap()
                                        .with_status(500), //unwrap
                                }
                            }
                            Payload::Json(data) => {
                                match Response::from_body(ResponseBody::Body(
                                    data.into_bytes(),
                                )) {
                                    Ok(r) => {
                                        headers.set(
                                            "Content-Type",
                                            "application/json",
                                        );
                                        r.with_headers(headers)
                                            .with_status(res_status)
                                    }
                                    _ => Response::empty()
                                        .unwrap()
                                        .with_status(500), //unwrap
                                }
                            }
                        }
                    }
                    Err(e) => {
                        match Response::from_body(ResponseBody::Body(
                            serde_json::to_string(&e)
                                .unwrap_or_else(|_| e.to_string())
                                .into_bytes(),
                        )) {
                            Ok(r) => r.with_status(500),
                            _ => Response::empty().unwrap().with_status(500), /* unwrap */
                        }
                    }
                };
                // clean up the scope
                disposer.dispose();
                runtime.dispose();
                res
            } else {
                match Response::from_body(ResponseBody::Body(
                    format!(
                        "Could not find a server function at the route {:?}. \
                         \n\nIt's likely that you need to call \
                         ServerFn::register_explicit() on the server function \
                         type, somewhere in your `main` function.",
                        url.path()
                    )
                    .into_bytes(),
                )) {
                    Ok(r) => r.with_status(400),
                    _ => Response::empty().unwrap().with_status(400), // unwrap
                }
            },
        )
    }
}

// pub fn render_app_to_stream<IV>(
//     options: LeptosOptions,
//     app_fn: impl Fn(leptos::Scope) -> IV + Clone + 'static,
//     method: Method,
// ) -> T
// where
//     T: Future<Output = Result<Response>> + 'a,
//     IV: IntoView,
// {
//     render_app_to_stream_with_context(options, |_cx| {}, app_fn, method)
// }

// pub fn render_app_to_stream_in_order<IV>(
//     options: LeptosOptions,
//     app_fn: impl Fn(leptos::Scope) -> IV + Clone + 'static,
//     method: Method,
// ) -> T
// where
//     T: Future<Output = Result<Response>> + 'a,
//     IV: IntoView,
// {
//     render_app_to_stream_in_order_with_context(
//         options,
//         |_cx| {},
//         app_fn,
//         method,
//     )
// }

// pub fn render_app_async<IV>(
//     options: LeptosOptions,
//     app_fn: impl Fn(leptos::Scope) -> IV + Clone + 'static,
//     method: Method,
// ) -> T
// where
//     T: Future<Output = Result<Response>> + 'a,
//     IV: IntoView,
// {
//     render_app_async_with_context(options, |_cx| {}, app_fn, method)
// }

// pub fn render_app_to_stream_with_context<IV>(
//     options: LeptosOptions,
//     additional_context: impl Fn(leptos::Scope) + 'static + Clone + Send,
//     app_fn: impl Fn(leptos::Scope) -> IV + Clone + 'static,
//     method: Method,
// ) -> T
// where
//     T: Future<Output = Result<Response>> + 'a,
//     IV: IntoView,
// {
//     render_app_to_stream_with_context_and_replace_blocks(
//         options,
//         additional_context,
//         app_fn,
//         method,
//         false,
//     )
// }

// pub fn render_app_to_stream_with_context_and_replace_blocks<IV>(
//     options: LeptosOptions,
//     additional_context: impl Fn(leptos::Scope) + 'static + Clone + Send,
//     app_fn: impl Fn(leptos::Scope) -> IV + Clone + 'static,
//     method: Method,
//     replace_blocks: bool,
// ) -> T
// where
//     T: Future<Output = Result<Response>> + 'a,
//     IV: IntoView,
// {

// }
