//! macOS: plans opened from Finder. macOS does not start an app with the file on its command
//! line; it sends the app an "open documents" Apple event (when the app starts, and again for
//! every file opened while it runs). winit does not pass that event on, so PlanCraft listens for
//! it itself and queues the files in `open_requests`.
//!
//! The handler has to be installed when the app is about to finish launching: AppKit installs its
//! own default handlers before that and delivers the launch's open-documents event right after it.
//! winit owns the application delegate, so PlanCraft watches the "will finish launching"
//! notification instead, which AppKit posts at that same moment.
//!
//! `Info.plist` declares the `.plancraft` file type (`xtask/src/mac.rs`), so Finder offers
//! PlanCraft for plans in the first place.

// Calling Apple's frameworks needs `unsafe`; each block says why it is sound.
#![allow(unsafe_code)]

use std::path::PathBuf;

use objc2::rc::Retained;
use objc2::runtime::NSObject;
use objc2::{ClassType, define_class, msg_send, sel};
use objc2_foundation::{NSAppleEventDescriptor, NSAppleEventManager, NSNotification, NSNotificationCenter};

/// Apple event codes (four-character codes): the core suite, its "open documents" event, and
/// the event's main parameter (the list of files).
const CORE_EVENT_CLASS: u32 = u32::from_be_bytes(*b"aevt");
const OPEN_DOCUMENTS: u32 = u32::from_be_bytes(*b"odoc");
const DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");
/// The kinds of item that name a file: a file URL, an alias, a bookmark and an FSRef. Anything
/// else is ignored: `fileURLValue` would also read plain text as an old-style (colon-separated)
/// path and turn it into a made-up file name.
const FILE_REFERENCES: [u32; 4] =
    [u32::from_be_bytes(*b"furl"), u32::from_be_bytes(*b"alis"), u32::from_be_bytes(*b"bmrk"), u32::from_be_bytes(*b"fsrf")];

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and this class adds no instance
    // variables and no `Drop`.
    #[unsafe(super(NSObject))]
    #[name = "PlanCraftOpenDocuments"]
    struct Handler;

    impl Handler {
        /// AppKit is about to finish launching: take over the open-documents event.
        // SAFETY: the signature matches the notification observer selector (one NSNotification
        // argument, no result).
        #[unsafe(method(willFinishLaunching:))]
        fn will_finish_launching(&self, _note: &NSNotification) {
            // SAFETY: `self` stays alive for the whole run (see `install`), and the selector names
            // a method of this class with the signature NSAppleEventManager expects
            // (event, reply).
            unsafe {
                NSAppleEventManager::sharedAppleEventManager().setEventHandler_andSelector_forEventClass_andEventID(
                    self,
                    sel!(openDocuments:withReply:),
                    CORE_EVENT_CLASS,
                    OPEN_DOCUMENTS,
                );
            }
        }

        /// Finder (or the Dock, or `open`) asks PlanCraft to open files.
        // SAFETY: the signature matches an Apple event handler selector (the event and the
        // reply descriptors, no result).
        #[unsafe(method(openDocuments:withReply:))]
        fn open_documents(&self, event: &NSAppleEventDescriptor, _reply: &NSAppleEventDescriptor) {
            // A panic must not unwind into AppKit's event dispatch.
            let queued = plancraft_core::guard::catch("reading files to open", || crate::open_requests::global().push(paths(event)));
            if let Err(p) = queued {
                eprintln!("plancraft: {p}");
            }
        }
    }
);

/// Starts listening for files to open; call before the window opens (before AppKit launches).
pub fn install() {
    // SAFETY: `new` on an NSObject subclass returns a fresh, initialised object.
    let handler: Retained<Handler> = unsafe { msg_send![Handler::class(), new] };
    // SAFETY: the observer stays alive for the whole run (it is never released, below), the
    // selector names its one-argument method, and the notification name is AppKit's own constant.
    unsafe {
        NSNotificationCenter::defaultCenter().addObserver_selector_name_object(
            &handler,
            sel!(willFinishLaunching:),
            Some(objc2_app_kit::NSApplicationWillFinishLaunchingNotification),
            None,
        );
    }
    // The notification center and the event manager keep only unretained references, so the
    // handler must outlive them: it lives as long as the app.
    std::mem::forget(handler);
}

/// The files an open-documents event names (a list of file URLs, or a single one).
fn paths(event: &NSAppleEventDescriptor) -> Vec<PathBuf> {
    let Some(direct) = event.paramDescriptorForKeyword(DIRECT_OBJECT) else {
        return Vec::new();
    };
    let count = direct.numberOfItems();
    let items: Vec<Retained<NSAppleEventDescriptor>> = if count < 1 {
        vec![direct]
    } else {
        let last = count.min(crate::open_requests::MAX_PENDING as isize);
        (1..=last).filter_map(|i| direct.descriptorAtIndex(i)).collect()
    };
    items
        .iter()
        .filter(|item| FILE_REFERENCES.contains(&item.descriptorType()))
        .filter_map(|item| item.fileURLValue())
        .filter(|url| url.isFileURL())
        .filter_map(|url| url.path())
        .map(|path| PathBuf::from(path.to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_foundation::{NSString, NSURL};

    /// An open-documents event as Finder sends it, with `direct` as its list of files.
    fn event(direct: Option<&NSAppleEventDescriptor>) -> Retained<NSAppleEventDescriptor> {
        // Return ID -1 and transaction ID 0: Apple's "generate one" and "any transaction".
        let event = NSAppleEventDescriptor::appleEventWithEventClass_eventID_targetDescriptor_returnID_transactionID(
            CORE_EVENT_CLASS,
            OPEN_DOCUMENTS,
            None,
            -1,
            0,
        );
        if let Some(direct) = direct {
            event.setParamDescriptor_forKeyword(direct, DIRECT_OBJECT);
        }
        event
    }

    fn file(path: &str) -> Retained<NSAppleEventDescriptor> {
        NSAppleEventDescriptor::descriptorWithFileURL(&NSURL::fileURLWithPath(&NSString::from_str(path)))
    }

    #[test]
    fn reads_every_file_in_the_list_in_order() {
        let list = NSAppleEventDescriptor::listDescriptor();
        list.insertDescriptor_atIndex(&file("/tmp/Plan A.plancraft"), 1);
        list.insertDescriptor_atIndex(&file("/tmp/Projekt Ü.plancraft"), 2);
        // macOS hands back accented names decomposed ("U" and a combining diaeresis), which its
        // file system treats as the same name.
        assert_eq!(paths(&event(Some(&list))), [PathBuf::from("/tmp/Plan A.plancraft"), PathBuf::from("/tmp/Projekt U\u{308}.plancraft")]);
    }

    #[test]
    fn reads_a_single_file_that_is_not_in_a_list() {
        assert_eq!(paths(&event(Some(&file("/tmp/One.plancraft")))), [PathBuf::from("/tmp/One.plancraft")]);
    }

    #[test]
    fn an_event_without_files_or_with_other_things_opens_nothing() {
        assert!(paths(&event(None)).is_empty());
        assert!(paths(&event(Some(&NSAppleEventDescriptor::listDescriptor()))).is_empty());
        let list = NSAppleEventDescriptor::listDescriptor();
        list.insertDescriptor_atIndex(&NSAppleEventDescriptor::descriptorWithString(&NSString::from_str("https://example.com/x")), 1);
        list.insertDescriptor_atIndex(&file("/tmp/Real.plancraft"), 2);
        assert_eq!(paths(&event(Some(&list))), [PathBuf::from("/tmp/Real.plancraft")]);
    }
}
