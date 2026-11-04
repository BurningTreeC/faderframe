//! AAF (Advanced Authoring Format) interchange: the sound of a timeline
//! exchanged with Pro Tools, Media Composer, Premiere and Resolve.
//!
//! The layers, bottom up:
//!
//! * [`cfb`] — the Microsoft Compound File container AAF files are;
//! * [`store`] — AAF's stored form of objects in it (properties streams,
//!   strong references as storages, collections with index streams, weak
//!   references through the referenced-properties table);
//! * [`model`] — the object model (classes, properties, types; generated
//!   tables) and [`meta`], the meta dictionary every file carries;
//! * [`timeline`] — what a composition says about sound, without AAF's
//!   machinery; [`write`] lays one out as mobs, slots, sequences and
//!   transitions, [`read`] flattens a file's top-level composition back
//!   into one.
//!
//! Written from the AMWA AAF Object Specification and Edit Protocol, not
//! from the AAF SDK. OMF is not read or written: OMF 2.0 files are Bento
//! containers with an object model of their own (four-letter classes,
//! other property sets), so they share neither layer below [`timeline`]
//! (see docs/ARCHITECTURE.md, "AAF interchange").

#![forbid(unsafe_code)]

pub mod cfb;
pub mod ids;
pub mod meta;
pub mod model;
pub mod read;
pub mod store;
pub mod timeline;
pub mod url;
pub mod wave;
pub mod write;
