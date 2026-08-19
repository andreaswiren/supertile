pub mod about;
pub mod highlight;
pub mod hover;
pub mod keys;
pub mod palette;
pub mod theme;
pub mod theme_editor;
pub mod tip;

/// Show the system colour chooser, returning the picked colour.
///
/// The Windows dialog rather than one of our own: it already has the spectrum,
/// the saturation ramp, a hex field and the custom-colour swatches, and it is
/// the picker every other Windows application uses. A hand-drawn one would be
/// worse and would need maintaining.
pub fn choose_colour(
    owner: windows::Win32::Foundation::HWND,
    initial: windows::Win32::Foundation::COLORREF,
) -> Option<windows::Win32::Foundation::COLORREF> {
    use windows::Win32::Foundation::COLORREF;
    use windows::Win32::UI::Controls::Dialogs::{
        ChooseColorW, CC_FULLOPEN, CC_RGBINIT, CHOOSECOLORW,
    };

    // The dialog writes the sixteen custom swatches back into this array, so it
    // must outlive the call and be writable.
    let mut custom = [COLORREF(0x00FF_FFFF); 16];
    let mut cc = CHOOSECOLORW {
        lStructSize: std::mem::size_of::<CHOOSECOLORW>() as u32,
        hwndOwner: owner,
        rgbResult: initial,
        lpCustColors: custom.as_mut_ptr(),
        // Open on the full picker: the collapsed form shows 48 basic colours
        // and hides the spectrum, which is not enough to theme with.
        Flags: CC_RGBINIT | CC_FULLOPEN,
        ..Default::default()
    };
    // SAFETY: `cc` is fully initialised with a correct lStructSize and the
    // custom-colour array outlives the call. ChooseColorW runs its own modal
    // loop; the caller holds no borrow of App across it.
    let ok = unsafe { ChooseColorW(&mut cc) };
    ok.as_bool().then_some(cc.rgbResult)
}
