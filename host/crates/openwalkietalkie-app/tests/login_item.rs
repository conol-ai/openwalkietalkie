//! The login item against the real `SMAppService` (§10.2, contract §1 M2).
//!
//! **A login item is a change to the operator's Mac**, and it outlives the
//! process that made it. So this suite has two halves and only one of them
//! runs by default:
//!
//! * the read is unconditional — `status()` registers nothing and can be
//!   asked as often as it likes;
//! * the **round trip** is behind [`LIVE`], because it registers this binary
//!   to open at login for as long as it takes to read the status back. It
//!   unregisters in the same test and asserts the status is **off** before it
//!   returns, and it refuses to start at all if the machine already has a
//!   login item it did not create — a test may not take away something the
//!   operator put there.
//!
//! Without the variable it prints a skip and its reason, the way every other
//! suite here treats a grant it does not have. A test binary is not the
//! installed app, so without it `register` would refuse anyway; the variable
//! is the one thing that lets it change this binary's own item, and never the
//! background service's.
use openwalkietalkie_app::login_item::{self, LIVE};
use openwalkietalkie_app::present::LoginItem;

#[test]
fn reading_the_login_item_status_registers_nothing_and_can_be_asked_twice() {
    let first = login_item::status();
    let second = login_item::status();
    assert_eq!(first, second);
    // It is one of the four Apple documents, or a code kept as itself.
    assert!(
        matches!(
            first,
            LoginItem::NotRegistered
                | LoginItem::Enabled
                | LoginItem::RequiresApproval
                | LoginItem::NotFound
                | LoginItem::Unknown(_)
        ),
        "{first:?}"
    );
    println!(
        "SMAppService.mainApp status: {first:?} (code {})",
        first.code()
    );
}

#[test]
fn registering_re_reads_the_status_and_leaves_nothing_behind() {
    if std::env::var(LIVE).as_deref() != Ok("1") {
        println!(
            "skip: this test registers a real login item on this Mac. Set {LIVE}=1 to run it; it \
             unregisters what it registers and asserts the status is back to 0 before it returns."
        );
        return;
    }
    let before = login_item::status();
    if before.registered() {
        println!(
            "skip: this Mac already has a login item for this binary ({before:?}). A test may not \
             unregister something the operator put there."
        );
        return;
    }

    // §1 M2: register, then **re-read**. The return value is not evidence —
    // on a fresh state it was `Ok` while the status was still 3 (notFound).
    let after = login_item::register().expect("register answered");
    println!("after register: {after:?} (code {})", after.code());

    // Whatever it said, put the machine back before asserting anything:
    // an assertion that fired here would leave the login item behind.
    let restored = login_item::unregister();
    let end = login_item::status();
    println!("after unregister: {end:?} (code {})", end.code());

    assert!(
        matches!(after, LoginItem::Enabled | LoginItem::RequiresApproval)
            || after == LoginItem::NotFound,
        "{after:?} is not one of the statuses a register can leave"
    );
    // `notFound` is exactly the M2 case and is not a failure of this code:
    // it is the reason the status is re-read at all. It is printed, not
    // asserted against, because it is a property of the machine.
    if after == LoginItem::NotFound {
        println!(
            "note: register() returned Ok and the status is notFound \u{2014} contract \u{a7}1 M2, \
             and why nothing reports success from the return value"
        );
    }
    assert!(restored.is_ok(), "unregister answered: {restored:?}");
    // The rule this suite exists for: **nothing is left behind.** In no case
    // is this binary left as a login item.
    //
    // The end status may be `notRegistered`(0) where it started at
    // `notFound`(3), and that is macOS and not a leak: `notFound` means the
    // system has no record of the item at all, and once it has heard of one
    // it reports the ordinary "off". Both are off and neither opens
    // anything at login.
    assert!(
        !end.registered(),
        "{end:?}: a test may never leave a login item on the operator's Mac"
    );
    assert!(
        matches!(end, LoginItem::NotRegistered | LoginItem::NotFound),
        "{end:?}: the item must end off, by one of the two words for it"
    );
}
