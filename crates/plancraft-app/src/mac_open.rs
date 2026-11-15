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
            let paths = plancraft_core::guard::catch("reading files to open", || paths(event)).unwrap_or_default();
            crate::open_requests::global().push(paths);
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
    items.iter().filter_map(|item| item.fileURLValue()).filter_map(|url| url.path()).map(|path| PathBuf::from(path.to_string())).collect()
}
