//! Hosted Hub links, ported from `app/hub-link.test.ts` (#89).
//!
//! A real hosted Hub (`minarbot-server --hub test-fixture`) runs with a
//! manual clock; node servers enroll through browser approval and talk to it
//! over real WebSockets through the kit's [`HubTransport`]. Received frames
//! can be rewritten and sent frames held or dropped, replacing the Node
//! suite's `receive` hook and `WebSocket.prototype.send` patches.
//!
//! [`HubTransport`]: minarbot_testkit::hosted_hub::HubTransport

mod cases;
mod effects;
mod fixture;
mod meter;
