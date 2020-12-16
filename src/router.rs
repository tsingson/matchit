//! HttpRouter is a lightweight high performance HTTP request router.
//! It is a Rust port of [`julienschmidt/httprouter`](https://github.com/julienschmidt/httprouter).
//!
//! This router supports variables in the routing pattern and matches against
//! the request method. It also scales better.
//!
//! The router is optimized for high performance and a small memory footprint.
//! It scales well even with very long paths and a large number of routes.
//! A compressing dynamic trie (radix tree) structure is used for efficient matching.
//!
//! Here is a simple example:
//! ```rust
//! use httprouter::{Router, Params};
//! use std::convert::Infallible;
//! use hyper::{Request, Response, Body};
//! 
//! async fn index(_: Request<Body>) -> Result<Response<Body>, Infallible> {
//!     Ok(Response::new("Hello, World!".into()))
//! }
//! 
//! async fn hello(req: Request<Body>) -> Result<Response<Body>, Infallible> {
//!     let params = req.extensions().get::<Params>().unwrap();
//!     Ok(Response::new(format!("Hello, {}", params.by_name("user").unwrap()).into()))
//! }
//! 
//! fn main() {
//!     let router = Router::default();
//!     router.get("/", index);
//!     router.get("/hello/:user", hello);
//! }
//! ```
//!
//! The router matches incoming requests by the request method and the path.
//! If a handle is registered for this path and method, the router delegates the
//! request to that function.
//! For the methods GET, POST, PUT, PATCH, DELETE and OPTIONS shortcut functions exist to
//! register handles, for all other methods router.Handle can be used.
//!
//! The registered path, against which the router matches incoming requests, can
//! contain two types of parameters:
//! ```ignore
//!  Syntax    Type
//!  :name     named parameter
//!  *name     catch-all parameter
//! ```
//!
//! Named parameters are dynamic path segments. They match anything until the
//! next '/' or the path end:
//! ```ignore
//!  Path: /blog/:category/:post
//! ```
//!
//!  Requests:
//! ```ignore
//!   /blog/rust/request-routers            match: category="rust", post="request-routers"
//!   /blog/rust/request-routers/           no match, but the router would redirect
//!   /blog/rust/                           no match
//!   /blog/rust/request-routers/comments   no match
//! ```
//!
//! Catch-all parameters match anything until the path end, including the
//! directory index (the '/' before the catch-all). Since they match anything
//! until the end, catch-all parameters must always be the final path element.
//!  Path: /files/*filepath
//!
//!  Requests:
//! ```ignore
//!   /files/                             match: filepath="/"
//!   /files/LICENSE                      match: filepath="/LICENSE"
//!   /files/templates/article.html       match: filepath="/templates/article.html"
//!   /files                              no match, but the router would redirect
//! ```
//! The value of parameters is saved as a slice of the Param struct, consisting
//! each of a key and a value. The slice is passed to the Handle func as a third
//! parameter.
//! There are two ways to retrieve the value of a parameter:
//!  1) by the name of the parameter
//! ```ignore
//!  let user = params.by_name("user") // defined by :user or *user
//! ```
//!  2) by the index of the parameter. This way you can also get the name (key)
//! ```ignore
//!  thirdKey   := params[2].key   // the name of the 3rd parameter
//!  thirdValue := params[2].value // the value of the 3rd parameter
//! ```
use crate::path::clean_path;
use crate::tree::{Node, RouteLookup};
use futures::future::{BoxFuture, Future};
use http::Method;
use std::collections::HashMap;
use std::str;

/// An asynchronous http handler
pub trait Handler {
  /// Errors produced by the handler.
  type Error: Sync + Send;

  /// Responses given by the handler.
  type Response: Sync + Send + Default;

  /// Requests recieved by the handler.
  type Request: Sync + Send + Default;

  /// The future response value.
  type Future: Future<Output = Result<Self::Response, Self::Error>> + Send;

  /// Handle the request and return the response asynchronously.
  fn handle(&self, req: Self::Request) -> Self::Future;
}

/// Router is container which can be used to dispatch requests to different
/// handler functions via configurable routes
pub struct Router<T> {
  pub trees: HashMap<Method, Node<T>>,

  /// Enables automatic redirection if the current route can't be matched but a
  /// handler for the path with (without) the trailing slash exists.
  /// For example if `/foo/` is requested but a route only exists for `/foo`, the
  /// client is redirected to /foo with http status code 301 for `GET` requests
  /// and 307 for all other request methods.
  pub redirect_trailing_slash: bool,

  /// If enabled, the router tries to fix the current request path, if no
  /// handle is registered for it.
  /// First superfluous path elements like `../` or `//` are removed.
  /// Afterwards the router does a case-insensitive lookup of the cleaned path.
  /// If a handle can be found for this route, the router makes a redirection
  /// to the corrected path with status code 301 for `GET` requests and 307 for
  /// all other request methods.
  /// For example `/FOO` and `/..//Foo` could be redirected to `/foo`.
  /// `redirect_trailing_slash` is independent of this option.
  pub redirect_fixed_path: bool,

  /// If enabled, the router checks if another method is allowed for the
  /// current route, if the current request can not be routed.
  /// If this is the case, the request is answered with `MethodNotAllowed`
  /// and HTTP status code 405.
  /// If no other Method is allowed, the request is delegated to the `NotFound`
  /// handler.
  pub handle_method_not_allowed: bool,

  /// If enabled, the router automatically replies to OPTIONS requests.
  /// Custom `OPTIONS` handlers take priority over automatic replies.
  pub handle_options: bool,

  /// An optional handler that is called on automatic `OPTIONS` requests.
  /// The handler is only called if `handle_options` is true and no `OPTIONS`
  /// handler for the specific path was set.
  /// The `Allowed` header is set before calling the handler.
  pub global_options: Option<T>,

  /// Cached value of global `(*)` allowed methods
  pub global_allowed: String,

  /// Configurable handler which is called when no matching route is
  /// found.
  pub not_found: Option<T>,

  /// Configurable handler which is called when a request
  /// cannot be routed and `handle_method_not_allowed` is true.
  /// The `Allow` header with allowed request methods is set before the handler
  /// is called.
  pub method_not_allowed: Option<T>,
}

impl<T> Router<T> {
  /// get is a shortcut for `router.handle(Method::GET, path, handle)`
  pub fn get(&mut self, path: &str, handle: T) {
    self.handle(Method::GET, path, handle);
  }

  /// head is a shortcut for `router.handle(Method::HEAD, path, handle)`
  pub fn head(&mut self, path: &str, handle: T) {
    self.handle(Method::HEAD, path, handle);
  }

  /// options is a shortcut for `router.handle(Method::OPTIONS, path, handle)`
  pub fn options(&mut self, path: &str, handle: T) {
    self.handle(Method::OPTIONS, path, handle);
  }

  /// post is a shortcut for `router.handle(Method::POST, path, handle)`
  pub fn post(&mut self, path: &str, handle: T) {
    self.handle(Method::POST, path, handle);
  }

  /// put is a shortcut for `router.handle(Method::POST, path, handle)`
  pub fn put(&mut self, path: &str, handle: T) {
    self.handle(Method::PUT, path, handle);
  }

  /// patch is a shortcut for `router.handle(Method::PATCH, path, handle)`
  pub fn patch(&mut self, path: &str, handle: T) {
    self.handle(Method::PATCH, path, handle);
  }

  /// delete is a shortcut for `router.handle(Method::DELETE, path, handle)`
  pub fn delete(&mut self, path: &str, handle: T) {
    self.handle(Method::DELETE, path, handle);
  }

  // Handle registers a new request handle with the given path and method.
  //
  // For GET, POST, PUT, PATCH and DELETE requests the respective shortcut
  // functions can be used.
  //
  // This function is intended for bulk loading and to allow the usage of less
  // frequently used, non-standardized or custom methods (e.g. for internal
  // communication with a proxy).
  pub fn handle(&mut self, method: Method, path: &str, handle: T) {
    if !path.starts_with('/') {
      panic!("path must begin with '/' in path '{}'", path);
    }

    self
      .trees
      .entry(method)
      .or_insert_with(Node::default)
      .add_route(path, handle);
  }

  /// Lookup allows the manual lookup of a method + path combo.
  /// This is e.g. useful to build a framework around this router.
  /// If the path was found, it returns the handle function and the path parameter
  /// values. Otherwise the third return value indicates whether a redirection to
  /// the same path with an extra / without the trailing slash should be performed.
  pub fn lookup(&mut self, method: &Method, path: &str) -> Result<RouteLookup<T>, bool> {
    self
      .trees
      .get_mut(method)
      .map(|n| n.get_value(path))
      .unwrap_or(Err(false))
  }

  /// [TODO]
  pub fn serve_files() {
    unimplemented!()
  }

  // returns a list of the allowed methods for a specific path
  // eg: 'GET, PATCH, OPTIONS'
  pub fn allowed(&self, path: &str, req_method: &Method) -> String {
    let mut allowed: Vec<String> = Vec::new();
    match path {
      "*" => {
        for method in self.trees.keys() {
          if method != Method::OPTIONS {
            allowed.push(method.to_string());
          }
        }
      }
      _ => {
        for method in self.trees.keys() {
          if method == req_method || method == Method::OPTIONS {
            continue;
          }

          if let Some(tree) = self.trees.get(method) {
            let handler = tree.get_value(path);

            if handler.is_ok() {
              allowed.push(method.to_string());
            }
          };
        }
      }
    };

    if !allowed.is_empty() {
      allowed.push(Method::OPTIONS.to_string())
    }

    allowed.join(", ")
  }
}

/// The default httprouter configuration
impl<T> Default for Router<T> {
  fn default() -> Self {
    Router {
      trees: HashMap::new(),
      redirect_trailing_slash: true,
      redirect_fixed_path: true,
      handle_method_not_allowed: true,
      handle_options: true,
      global_allowed: String::new(),
      global_options: None,
      method_not_allowed: None,
      not_found: None,
    }
  }
}

#[cfg(feature = "hyper-server")]
pub mod hyper_server {
  use super::*;
  use hyper::{header, Body, Request, Response, StatusCode};
  use std::convert::Infallible;
  use std::marker::PhantomData;

  pub struct HandlerS<F, O>
  where
    F: Fn(Request<Body>) -> O,
    O: Future<Output = Result<Response<Body>, Infallible>> + Send,
  {
    handler: F,
    _t: PhantomData<O>,
  }

  impl<F, O> HandlerS<F, O>
  where
    F: Fn(Request<Body>) -> O,
    O: Future<Output = Result<Response<Body>, Infallible>> + Send,
  {
    /// Create a `Handler` from an asynchronous user-defined handler function (`Factory`)
    pub fn new(handler: F) -> Self {
      HandlerS {
        handler,
        _t: PhantomData,
      }
    }
  }

  impl<F, O> Handler for HandlerS<F, O>
  where
    F: Fn(Request<Body>) -> O,
    O: Future<Output = Result<Response<Body>, Infallible>> + Send + 'static,
  {
    type Request = Request<Body>;
    type Response = Response<Body>;
    type Error = Infallible;
    type Future = BoxFuture<'static, Result<Response<Body>, Infallible>>;

    fn handle(
      &self,
      req: Self::Request,
    ) -> BoxFuture<'static, Result<Self::Response, Self::Error>> {
      Box::pin((self.handler)(req))
    }
  }

  pub type BoxedHandler = Box<
    dyn Handler<
        Request = Request<Body>,
        Response = Response<Body>,
        Error = Infallible,
        Future = BoxFuture<'static, Result<Response<Body>, Infallible>>,
      > + Send
      + Sync,
  >;

  impl Router<BoxedHandler> {
    /// Serve the router on a hyper server
    pub async fn serve(&self, mut req: Request<Body>) -> Result<Response<Body>, Infallible> {
      let root = self.trees.get(req.method());
      let path = req.uri().path();
      if let Some(root) = root {
        match root.get_value(path) {
          Ok(lookup) => {
            req.extensions_mut().insert(lookup.params);
            return lookup.value.handle(req).await;
          }
          Err(tsr) => {
            if req.method() != Method::CONNECT && path != "/" {
              let code = match *req.method() {
                // Moved Permanently, request with GET method
                Method::GET => StatusCode::MOVED_PERMANENTLY,
                // Permanent Redirect, request with same method
                _ => StatusCode::PERMANENT_REDIRECT,
              };

              if tsr && self.redirect_trailing_slash {
                let path = if path.len() > 1 && path.ends_with('/') {
                  path[..path.len() - 1].to_string()
                } else {
                  path.to_string() + "/"
                };

                return Ok(
                  Response::builder()
                    .header(header::LOCATION, path.as_str())
                    .status(code)
                    .body(Body::empty())
                    .unwrap(),
                );
              };

              if self.redirect_fixed_path {
                if let Some(fixed_path) =
                  root.find_case_insensitive_path(&clean_path(path), self.redirect_trailing_slash)
                {
                  return Ok(
                    Response::builder()
                      .header(header::LOCATION, fixed_path.as_str())
                      .status(code)
                      .body(Body::empty())
                      .unwrap(),
                  );
                }
              };
            };
          }
        }
      };

      if req.method() == Method::OPTIONS && self.handle_options {
        let allow = self.allowed(path, &Method::OPTIONS);
        if allow != "" {
          match &self.global_options {
            Some(handler) => return handler.handle(req).await,
            None => {
              return Ok(
                Response::builder()
                  .header(header::ALLOW, allow)
                  .body(Body::empty())
                  .unwrap(),
              );
            }
          };
        }
      } else if self.handle_method_not_allowed {
        let allow = self.allowed(path, req.method());

        if !allow.is_empty() {
          if let Some(ref handler) = self.method_not_allowed {
            return handler.handle(req).await;
          }
          return Ok(
            Response::builder()
              .header(header::ALLOW, allow)
              .status(StatusCode::METHOD_NOT_ALLOWED)
              .body(Body::empty())
              .unwrap(),
          );
        }
      };

      match &self.not_found {
        Some(handler) => handler.handle(req).await,
        None => Ok(Response::builder().status(404).body(Body::empty()).unwrap()),
      }
    }
  }
}