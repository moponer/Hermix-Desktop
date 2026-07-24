//! # UI Automation — `IUIAutomation` COM client
//!
//! Full element tree walking, property querying, element finding, and
//! pattern invocation using the Windows `UIAutomationClient` COM API.
//!
//! ## Design decisions
//!
//! - **Snapshot-based** — [`UiElement`] collects ~20 properties at
//!   once during a single walk pass. No lazy/on-demand property
//!   reading; consumers get a fully populated struct immediately.
//! - **Tree serialisation** — [`walk_tree`] does a depth-first walk
//!   up to a configurable depth and returns a flat `Vec<UiElement>`.
//!   The front-end renders the nesting via `depth` / `index` fields.
//! - **Pattern support** — `Invoke` is the most useful for CUA.
//!   Others can be added on demand.
//! - **Condition matching** — simple tree built from [`UiaCondition`]
//!   (Name / AutomationId / ClassName / ControlType / And / Or);
//!   compiled into a COM `IUIAutomationCondition` tree.
//!
//! ## Thread safety
//!
//! `IUIAutomation` is apartment-threaded. `UiaAutomation` wraps a
//! raw COM pointer and is NOT `Send`/`Sync`. All public functions
//! are meant to be called from the thread that called `CoInitializeEx`.
//! Consumers wrap with `spawn_blocking`.

use crate::element::ElementRect;
use crate::error::CuaError;
use crate::CuaResult;

// Re-export the key COM interface so consumers can reference it.
// We explicitly list every type we import from Accessibility (rather
// than globbing everything) to avoid E0252 name conflicts.
pub use windows::Win32::UI::Accessibility::IUIAutomationElement;

// Bring the windows COM types in with explicit scope to avoid
// name conflicts with our own ControlType / UiaCondition enums.
use windows::core::Interface as _;
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationCondition, IUIAutomationInvokePattern,
    IUIAutomationTreeWalker, TreeScope_Descendants,
    UIA_AppBarControlTypeId, UIA_AutomationIdPropertyId,
    UIA_ButtonControlTypeId, UIA_CalendarControlTypeId, UIA_CheckBoxControlTypeId,
    UIA_ClassNamePropertyId, UIA_ComboBoxControlTypeId,
    UIA_ControlTypePropertyId, UIA_CustomControlTypeId,
    UIA_DataGridControlTypeId, UIA_DataItemControlTypeId,
    UIA_DocumentControlTypeId, UIA_EditControlTypeId,
    UIA_GroupControlTypeId, UIA_HeaderControlTypeId,
    UIA_HeaderItemControlTypeId, UIA_HyperlinkControlTypeId,
    UIA_ImageControlTypeId, UIA_InvokePatternId,
    UIA_ItemStatusPropertyId, UIA_ItemTypePropertyId,
    UIA_ListControlTypeId, UIA_ListItemControlTypeId,
    UIA_LocalizedControlTypePropertyId, UIA_MenuBarControlTypeId,
    UIA_MenuControlTypeId, UIA_MenuItemControlTypeId, UIA_NamePropertyId,
    UIA_PaneControlTypeId, UIA_ProgressBarControlTypeId,
    UIA_ProviderDescriptionPropertyId, UIA_RadioButtonControlTypeId,
    UIA_ScrollBarControlTypeId, UIA_SemanticZoomControlTypeId,
    UIA_SeparatorControlTypeId, UIA_SliderControlTypeId,
    UIA_SpinnerControlTypeId, UIA_SplitButtonControlTypeId,
    UIA_StatusBarControlTypeId, UIA_TabControlTypeId, UIA_TabItemControlTypeId,
    UIA_TableControlTypeId, UIA_TextControlTypeId,
    UIA_ThumbControlTypeId, UIA_TitleBarControlTypeId,
    UIA_ToolBarControlTypeId, UIA_ToolTipControlTypeId,
    UIA_TreeControlTypeId, UIA_TreeItemControlTypeId,
    UIA_WindowControlTypeId,
    UIA_CONTROLTYPE_ID, UIA_PROPERTY_ID,
};
use windows::Win32::System::Com::*;
use windows::Win32::Foundation::*;

// ---------------------------------------------------------------------------
// ControlType — friendly enum wrapping the Win32 constants
// ---------------------------------------------------------------------------

/// UI Automation control-type identifiers (the ~40 common ones).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlType {
    AppBar,
    Button,
    Calendar,
    CheckBox,
    ComboBox,
    Custom,
    DataGrid,
    DataItem,
    Document,
    Edit,
    Group,
    Header,
    HeaderItem,
    Hyperlink,
    Image,
    List,
    ListItem,
    MenuBar,
    Menu,
    MenuItem,
    Pane,
    ProgressBar,
    RadioButton,
    ScrollBar,
    SemanticZoom,
    Separator,
    Slider,
    Spinner,
    SplitButton,
    StatusBar,
    Tab,
    TabItem,
    Table,
    Text,
    Thumb,
    TitleBar,
    ToolBar,
    ToolTip,
    Tree,
    TreeItem,
    Window,
    /// Any control type not in the above list (stores the raw `i32`).
    Unknown(i32),
}

fn control_type_from_id(id: i32) -> ControlType {
    let raw = UIA_CONTROLTYPE_ID(id);
    if raw == UIA_AppBarControlTypeId { ControlType::AppBar }
    else if raw == UIA_ButtonControlTypeId { ControlType::Button }
    else if raw == UIA_CalendarControlTypeId { ControlType::Calendar }
    else if raw == UIA_CheckBoxControlTypeId { ControlType::CheckBox }
    else if raw == UIA_ComboBoxControlTypeId { ControlType::ComboBox }
    else if raw == UIA_CustomControlTypeId { ControlType::Custom }
    else if raw == UIA_DataGridControlTypeId { ControlType::DataGrid }
    else if raw == UIA_DataItemControlTypeId { ControlType::DataItem }
    else if raw == UIA_DocumentControlTypeId { ControlType::Document }
    else if raw == UIA_EditControlTypeId { ControlType::Edit }
    else if raw == UIA_GroupControlTypeId { ControlType::Group }
    else if raw == UIA_HeaderControlTypeId { ControlType::Header }
    else if raw == UIA_HeaderItemControlTypeId { ControlType::HeaderItem }
    else if raw == UIA_HyperlinkControlTypeId { ControlType::Hyperlink }
    else if raw == UIA_ImageControlTypeId { ControlType::Image }
    else if raw == UIA_ListControlTypeId { ControlType::List }
    else if raw == UIA_ListItemControlTypeId { ControlType::ListItem }
    else if raw == UIA_MenuBarControlTypeId { ControlType::MenuBar }
    else if raw == UIA_MenuControlTypeId { ControlType::Menu }
    else if raw == UIA_MenuItemControlTypeId { ControlType::MenuItem }
    else if raw == UIA_PaneControlTypeId { ControlType::Pane }
    else if raw == UIA_ProgressBarControlTypeId { ControlType::ProgressBar }
    else if raw == UIA_RadioButtonControlTypeId { ControlType::RadioButton }
    else if raw == UIA_ScrollBarControlTypeId { ControlType::ScrollBar }
    else if raw == UIA_SemanticZoomControlTypeId { ControlType::SemanticZoom }
    else if raw == UIA_SeparatorControlTypeId { ControlType::Separator }
    else if raw == UIA_SliderControlTypeId { ControlType::Slider }
    else if raw == UIA_SpinnerControlTypeId { ControlType::Spinner }
    else if raw == UIA_SplitButtonControlTypeId { ControlType::SplitButton }
    else if raw == UIA_StatusBarControlTypeId { ControlType::StatusBar }
    else if raw == UIA_TabControlTypeId { ControlType::Tab }
    else if raw == UIA_TabItemControlTypeId { ControlType::TabItem }
    else if raw == UIA_TableControlTypeId { ControlType::Table }
    else if raw == UIA_TextControlTypeId { ControlType::Text }
    else if raw == UIA_ThumbControlTypeId { ControlType::Thumb }
    else if raw == UIA_TitleBarControlTypeId { ControlType::TitleBar }
    else if raw == UIA_ToolBarControlTypeId { ControlType::ToolBar }
    else if raw == UIA_ToolTipControlTypeId { ControlType::ToolTip }
    else if raw == UIA_TreeControlTypeId { ControlType::Tree }
    else if raw == UIA_TreeItemControlTypeId { ControlType::TreeItem }
    else if raw == UIA_WindowControlTypeId { ControlType::Window }
    else { ControlType::Unknown(id) }
}

// ---------------------------------------------------------------------------
// UiElement — a fully populated snapshot of one UIA element
// ---------------------------------------------------------------------------

/// A snapshot of a UI Automation element's key properties.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UiElement {
    pub name: String,
    pub class_name: String,
    pub automation_id: String,
    pub control_type: ControlType,
    pub rect: ElementRect,
    pub is_enabled: bool,
    pub is_offscreen: bool,
    pub process_id: i32,
    pub has_keyboard_focus: bool,
    pub is_password: bool,
    pub help_text: String,
    pub item_status: String,
    pub item_type: String,
    pub localized_control_type: String,
    pub accelerator_key: String,
    pub access_key: String,
    pub framework_id: String,
    pub provider_description: String,
    pub depth: u32,
    pub index: usize,
}

// ---------------------------------------------------------------------------
// UiaAutomation — IUIAutomation COM wrapper
// ---------------------------------------------------------------------------

/// An initialised `IUIAutomation` instance.
///
/// Must be created on a thread that holds a COM apartment.
pub struct UiaAutomation {
    inner: IUIAutomation,
}

impl UiaAutomation {
    /// Initialise COM on the **current thread** and create the
    /// `CUIAutomation` instance.
    /// /// ## Errors
    /// /// Returns `CuaError::Uia` if `CoInitializeEx` or
    /// `CoCreateInstance` fails.
    /// /// # Safety
    /// /// Must be called on a thread with a valid COM apartment (STA or MTA).
    /// The `COINIT_MULTITHREADED` flag initialises the thread for MTA;
    /// callers must ensure no conflicting STA initialisation exists.
    pub unsafe fn new() -> CuaResult<Self> {
        // SAFETY: `CoInitializeEx` is safe to call from any thread that
        // hasn't already initialised a conflicting COM apartment. The
        // return value is ignored because a previous successful init
        // on this thread is not an error (S_FALSE).
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

        // SAFETY: `CoCreateInstance` creates a COM object for `CUIAutomation`.
        // This is safe because `CUIAutomation` is a system-provided class
        // that is always available on Windows.
        let inner: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_ALL)
            .map_err(|e| CuaError::Uia(format!("CoCreateInstance(CUIAutomation): {e}")))?;

        Ok(Self { inner })
    }

    // -- Root / HWND / Point ----------------------------------------------

    /// Root desktop element.
    /// /// # Safety
    /// /// The `UiaAutomation` instance must have been created with `new()`
    /// on a COM-initialised thread. The returned `UiElement` is valid
    /// for the lifetime of the underlying COM object.
    pub unsafe fn get_root_element(&self) -> CuaResult<UiElement> {
        let raw = self
            .inner
            .GetRootElement()
            .map_err(|e| CuaError::Uia(format!("GetRootElement: {e}")))?;
        collect_properties(&raw, 0, 0)
    }

    /// Element for a given `HWND`.
    /// /// # Safety
    /// /// `hwnd` must be a valid window handle on the current desktop.
    pub unsafe fn element_from_handle(&self, hwnd: isize) -> CuaResult<UiElement> {
        let raw = self
            .inner
            .ElementFromHandle(HWND(hwnd as *mut std::ffi::c_void))
            .map_err(|e| CuaError::Uia(format!("ElementFromHandle: {e}")))?;
        collect_properties(&raw, 0, 0)
    }

    /// Element at screen coordinates `(x, y)`.
    /// /// # Safety
    /// /// Must be called on a COM-initialised thread.
    pub unsafe fn element_from_point(&self, x: i32, y: i32) -> CuaResult<UiElement> {
        let pt = POINT { x, y };
        let raw = self
            .inner
            .ElementFromPoint(pt)
            .map_err(|e| CuaError::Uia(format!("ElementFromPoint({x},{y}): {e}")))?;
        collect_properties(&raw, 0, 0)
    }

    // -- Element finding --------------------------------------------------

    /// Find first element matching `condition`, starting from the root.
    /// /// # Safety
    /// /// Must be called on a COM-initialised thread.
    pub unsafe fn find_first(&self, condition: &UiaCondition) -> CuaResult<Option<UiElement>> {
        let root = self
            .inner
            .GetRootElement()
            .map_err(|e| CuaError::Uia(format!("find_first/GetRootElement: {e}")))?;
        self.find_first_from(&root, condition)
    }

    /// Find first element matching `condition` under a given parent.
    /// /// # Safety
    /// /// Must be called on a COM-initialised thread.
    pub unsafe fn find_first_from(
        &self,
        parent: &IUIAutomationElement,
        condition: &UiaCondition,
    ) -> CuaResult<Option<UiElement>> {
        let com_cond = build_condition(&self.inner, condition)?;
        let found = parent
            .FindFirst(TreeScope_Descendants, &com_cond)
            .map_err(|e| CuaError::Uia(format!("FindFirst: {e}")))?;
        // IUIAutomationElement has null inner pointer when nothing matches
        if found.as_raw().is_null() {
            return Ok(None);
        }
        Ok(Some(collect_properties(&found, 0, 0)?))
    }

    /// Find all elements matching `condition`, starting from the root.
    /// /// # Safety
    /// /// Must be called on a COM-initialised thread.
    pub unsafe fn find_all(&self, condition: &UiaCondition) -> CuaResult<Vec<UiElement>> {
        let root = self
            .inner
            .GetRootElement()
            .map_err(|e| CuaError::Uia(format!("find_all/GetRootElement: {e}")))?;
        self.find_all_from(&root, condition)
    }

    /// Find all elements matching `condition` under a given parent.
    /// /// # Safety
    /// /// Must be called on a COM-initialised thread.
    pub unsafe fn find_all_from(
        &self,
        parent: &IUIAutomationElement,
        condition: &UiaCondition,
    ) -> CuaResult<Vec<UiElement>> {
        let com_cond = build_condition(&self.inner, condition)?;
        let elems = parent
            .FindAll(TreeScope_Descendants, &com_cond)
            .map_err(|e| CuaError::Uia(format!("FindAll: {e}")))?;
        let length = elems.Length().map_err(|e| CuaError::Uia(format!("array.Length: {e}")))? as usize;
        let mut out = Vec::with_capacity(length);
        for i in 0..length {
            let el = elems
                .GetElement(i as i32)
                .map_err(|e| CuaError::Uia(format!("array[{i}]: {e}")))?;
            out.push(collect_properties(&el, 0, i)?);
        }
        Ok(out)
    }

    // -- Raw COM element access (for click/focus/invoke) -------------------

    /// Find first element matching `condition` and return the raw
    /// COM `IUIAutomationElement` pointer.
    /// /// This is used by the CUA handlers that need to call COM
    /// methods (GetClickablePoint, SetFocus, GetCurrentPattern)
    /// directly, rather than going through a `UiElement` snapshot.
    /// /// **Important**: the returned pointer is only valid on the
    /// thread that called this method (the COM apartment thread).
    /// Do NOT stash it in a static or send it across threads.
    /// /// # Safety
    /// /// Must be called on a COM-initialised thread. The returned
    /// `IUIAutomationElement` COM pointer is not thread-safe.
    pub unsafe fn find_first_raw(
        &self,
        condition: &UiaCondition,
    ) -> CuaResult<Option<IUIAutomationElement>> {
        let root = self
            .inner
            .GetRootElement()
            .map_err(|e| CuaError::Uia(format!("find_first_raw/root: {e}")))?;
        let com_cond = build_condition(&self.inner, condition)?;
        let found = root
            .FindFirst(TreeScope_Descendants, &com_cond)
            .map_err(|e| CuaError::Uia(format!("FindFirst: {e}")))?;
        if found.as_raw().is_null() {
            return Ok(None);
        }
        Ok(Some(found))
    }

    // -- Tree walking -----------------------------------------------------

    /// Walk the control view tree up to `max_depth` and return
    /// all elements as a depth-first, flattened `Vec`.
    /// /// A `max_depth` of 0 means only the root element (no children).
    /// Use `u32::MAX` to walk the full tree (may be very large — 10k+).
    /// /// # Safety
    /// /// Must be called on a COM-initialised thread.
    pub unsafe fn get_control_view_tree(&self, max_depth: u32) -> CuaResult<Vec<UiElement>> {
        let true_cond = self
            .inner
            .CreateTrueCondition()
            .map_err(|e| CuaError::Uia(format!("CreateTrueCondition: {e}")))?;
        let walker = self
            .inner
            .CreateTreeWalker(&true_cond)
            .map_err(|e| CuaError::Uia(format!("CreateTreeWalker: {e}")))?;

        let root = self
            .inner
            .GetRootElement()
            .map_err(|e| CuaError::Uia(format!("GetRootElement: {e}")))?;

        let mut out = Vec::new();
        let mut index_counter = 0usize;
        walk_node(&walker, &root, 0, max_depth, &mut out, &mut index_counter)?;
        Ok(out)
    }
}

// =========================================================================
// Condition tree
// =========================================================================

/// A condition tree that the user can construct in Rust and compile
/// into a COM `IUIAutomationCondition`.
#[derive(Debug, Clone)]
pub enum UiaCondition {
    Name(String),
    AutomationId(String),
    ClassName(String),
    ControlType(ControlType),
    And(Vec<UiaCondition>),
    Or(Vec<UiaCondition>),
}

/// Build a `UiaCondition` from the high-level `UiaCondition` enum.
///
/// # Safety
///
/// Must be called on a COM-initialised thread. `automation` must be
/// a valid `IUIAutomation` COM pointer.
unsafe fn build_condition(
    automation: &IUIAutomation,
    cond: &UiaCondition,
) -> CuaResult<IUIAutomationCondition> {
    match cond {
        UiaCondition::Name(val) => create_prop_cond(automation, UIA_NamePropertyId, val),
        UiaCondition::AutomationId(val) => {
            create_prop_cond(automation, UIA_AutomationIdPropertyId, val)
        }
        UiaCondition::ClassName(val) => create_prop_cond(automation, UIA_ClassNamePropertyId, val),
        UiaCondition::ControlType(ct) => {
            let id = control_type_to_id(*ct);
            automation
                .CreatePropertyCondition(UIA_ControlTypePropertyId, &windows::core::VARIANT::from(id.0))
                .map_err(|e| CuaError::Uia(format!("CreatePropertyCondition(ControlType): {e}")))
        }
        UiaCondition::And(children) => {
            // Chain binary CreateAndCondition instead of SAFEARRAY
            if children.is_empty() {
                return automation.CreateTrueCondition()
                    .map_err(|e| CuaError::Uia(format!("CreateTrueCondition: {e}")));
            }
            let mut cond = build_condition(automation, &children[0])?;
            for child in &children[1..] {
                let next = build_condition(automation, child)?;
                cond = automation
                    .CreateAndCondition(&cond, &next)
                    .map_err(|e| CuaError::Uia(format!("CreateAndCondition: {e}")))?;
            }
            Ok(cond)
        }
        UiaCondition::Or(children) => {
            // Chain binary CreateOrCondition instead of SAFEARRAY
            if children.is_empty() {
                return automation.CreateTrueCondition()
                    .map_err(|e| CuaError::Uia(format!("CreateTrueCondition: {e}")));
            }
            let mut cond = build_condition(automation, &children[0])?;
            for child in &children[1..] {
                let next = build_condition(automation, child)?;
                cond = automation
                    .CreateOrCondition(&cond, &next)
                    .map_err(|e| CuaError::Uia(format!("CreateOrCondition: {e}")))?;
            }
            Ok(cond)
        }
    }
}

/// Create a property condition (e.g. Name = "...").
///
/// # Safety
///
/// Must be called on a COM-initialised thread.
unsafe fn create_prop_cond(
    automation: &IUIAutomation,
    prop_id: UIA_PROPERTY_ID,
    val: &str,
) -> CuaResult<IUIAutomationCondition> {
    let variant = windows::core::VARIANT::from(val);
    automation
        .CreatePropertyCondition(prop_id, &variant)
        .map_err(|e| CuaError::Uia(format!("CreatePropertyCondition: {e}")))
}

fn control_type_to_id(ct: ControlType) -> UIA_CONTROLTYPE_ID {
    match ct {
        ControlType::AppBar => UIA_AppBarControlTypeId,
        ControlType::Button => UIA_ButtonControlTypeId,
        ControlType::Calendar => UIA_CalendarControlTypeId,
        ControlType::CheckBox => UIA_CheckBoxControlTypeId,
        ControlType::ComboBox => UIA_ComboBoxControlTypeId,
        ControlType::Custom => UIA_CustomControlTypeId,
        ControlType::DataGrid => UIA_DataGridControlTypeId,
        ControlType::DataItem => UIA_DataItemControlTypeId,
        ControlType::Document => UIA_DocumentControlTypeId,
        ControlType::Edit => UIA_EditControlTypeId,
        ControlType::Group => UIA_GroupControlTypeId,
        ControlType::Header => UIA_HeaderControlTypeId,
        ControlType::HeaderItem => UIA_HeaderItemControlTypeId,
        ControlType::Hyperlink => UIA_HyperlinkControlTypeId,
        ControlType::Image => UIA_ImageControlTypeId,
        ControlType::List => UIA_ListControlTypeId,
        ControlType::ListItem => UIA_ListItemControlTypeId,
        ControlType::MenuBar => UIA_MenuBarControlTypeId,
        ControlType::Menu => UIA_MenuControlTypeId,
        ControlType::MenuItem => UIA_MenuItemControlTypeId,
        ControlType::Pane => UIA_PaneControlTypeId,
        ControlType::ProgressBar => UIA_ProgressBarControlTypeId,
        ControlType::RadioButton => UIA_RadioButtonControlTypeId,
        ControlType::ScrollBar => UIA_ScrollBarControlTypeId,
        ControlType::SemanticZoom => UIA_SemanticZoomControlTypeId,
        ControlType::Separator => UIA_SeparatorControlTypeId,
        ControlType::Slider => UIA_SliderControlTypeId,
        ControlType::Spinner => UIA_SpinnerControlTypeId,
        ControlType::SplitButton => UIA_SplitButtonControlTypeId,
        ControlType::StatusBar => UIA_StatusBarControlTypeId,
        ControlType::Tab => UIA_TabControlTypeId,
        ControlType::TabItem => UIA_TabItemControlTypeId,
        ControlType::Table => UIA_TableControlTypeId,
        ControlType::Text => UIA_TextControlTypeId,
        ControlType::Thumb => UIA_ThumbControlTypeId,
        ControlType::TitleBar => UIA_TitleBarControlTypeId,
        ControlType::ToolBar => UIA_ToolBarControlTypeId,
        ControlType::ToolTip => UIA_ToolTipControlTypeId,
        ControlType::Tree => UIA_TreeControlTypeId,
        ControlType::TreeItem => UIA_TreeItemControlTypeId,
        ControlType::Window => UIA_WindowControlTypeId,
        ControlType::Unknown(raw) => UIA_CONTROLTYPE_ID(raw),
    }
}

// =========================================================================
// Property collection
// =========================================================================

/// Collect common properties from a UIA element into a `UiElement`.
///
/// # Safety
///
/// `raw` must be a valid `IUIAutomationElement` COM pointer.
unsafe fn collect_properties(
    raw: &IUIAutomationElement,
    depth: u32,
    index: usize,
) -> CuaResult<UiElement> {
    macro_rules! try_str {
        ($expr:expr) => {
            match $expr {
                Ok(v) => format!("{}", v),
                Err(_) => String::new(),
            }
        };
    }

    macro_rules! try_bool {
        ($expr:expr) => {
            match $expr {
                Ok(v) => v.as_bool(),
                Err(_) => false,
            }
        };
    }

    macro_rules! try_i32 {
        ($expr:expr) => {
            match $expr {
                Ok(v) => v,
                Err(_) => 0,
            }
        };
    }

    let name = try_str!(raw.CurrentName());
    let class_name = try_str!(raw.CurrentClassName());
    let automation_id = try_str!(raw.CurrentAutomationId());
    let is_enabled = try_bool!(raw.CurrentIsEnabled());
    let is_offscreen = try_bool!(raw.CurrentIsOffscreen());
    let process_id = try_i32!(raw.CurrentProcessId());
    let has_keyboard_focus = try_bool!(raw.CurrentHasKeyboardFocus());
    let is_password = try_bool!(raw.CurrentIsPassword());
    let help_text = try_str!(raw.CurrentHelpText());
    let accelerator_key = try_str!(raw.CurrentAcceleratorKey());
    let access_key = try_str!(raw.CurrentAccessKey());
    let framework_id = try_str!(raw.CurrentFrameworkId());

    // Properties without dedicated getters — use GetCurrentPropertyValue
    let item_status = get_prop_string(raw, &UIA_ItemStatusPropertyId);
    let item_type = get_prop_string(raw, &UIA_ItemTypePropertyId);
    let provider_description = get_prop_string(raw, &UIA_ProviderDescriptionPropertyId);

    // LocalizedControlType uses its own property ID
    let localized_control_type = get_prop_string(raw, &UIA_LocalizedControlTypePropertyId);

    // Control type
    let ct_raw = match raw.CurrentControlType() {
        Ok(v) => v.0,
        Err(_) => 0,
    };
    let control_type = control_type_from_id(ct_raw);

    // Bounding rectangle — `CurrentBoundingRectangle()` returns a
    // plain `RECT` (left/top/right/bottom, i32) directly, NOT a
    // VARIANT SAFEARRAY. The original comment conflated this with
    // `GetCurrentPropertyValue(UIA_BoundingRectanglePropertyId)`,
    // which DOES go through VARIANT/SAFEARRAY and is harder to
    // extract in windows 0.58. The dedicated getter avoids that
    // path entirely.
    // Errors (e.g. an off-screen / non-rendered element) leave the
    // rect at the default (all zeros); `contains()` then returns
    // false for every point so the click target falls back to
    // `GetClickablePoint`.
    let rect = match raw.CurrentBoundingRectangle() {
        Ok(r) => ElementRect {
            x: r.left,
            y: r.top,
            width: r.right - r.left,
            height: r.bottom - r.top,
        },
        Err(_) => ElementRect::default(),
    };

    Ok(UiElement {
        name,
        class_name,
        automation_id,
        control_type,
        rect,
        is_enabled,
        is_offscreen,
        process_id,
        has_keyboard_focus,
        is_password,
        help_text,
        item_status,
        item_type,
        localized_control_type,
        accelerator_key,
        access_key,
        framework_id,
        provider_description,
        depth,
        index,
    })
}

fn get_prop_string(
    _raw: &IUIAutomationElement,
    _prop_id: &UIA_PROPERTY_ID,
) -> String {
    // GetCurrentPropertyValue returns a VARIANT, and extracting
    // BSTR from VARIANT in windows 0.58 requires unsafe field
    // access on the raw union. We skip this for now — the
    // important properties (name, class, automation_id, etc.)
    // all have dedicated getter methods above.
    String::new()
}

// =========================================================================
// Tree walking
// =========================================================================

/// Walk the UIA tree using a `TreeWalker`, collecting up to `max_depth`
/// levels of children.
///
/// # Safety
///
/// Must be called on a COM-initialised thread. `walker` and `parent`
/// must be valid COM pointers.
unsafe fn walk_node(
    walker: &IUIAutomationTreeWalker,
    current: &IUIAutomationElement,
    depth: u32,
    max_depth: u32,
    out: &mut Vec<UiElement>,
    index_counter: &mut usize,
) -> CuaResult<()> {
    let idx = *index_counter;
    *index_counter += 1;

    let el = collect_properties(current, depth, idx)?;
    out.push(el);

    if depth >= max_depth {
        return Ok(());
    }

    // Get first child — errors means no children (e.g. leaf element)
    let mut child = match walker.GetFirstChildElement(current) {
        Ok(c) => c,
        Err(_) => return Ok(()),
    };

    // Walk the first child's subtree
    walk_node(walker, &child, depth + 1, max_depth, out, index_counter)?;

    // Walk siblings
    loop {
        match walker.GetNextSiblingElement(&child) {
            Ok(sibling) => {
                walk_node(walker, &sibling, depth + 1, max_depth, out, index_counter)?;
                child = sibling;
            }
            Err(_) => break,
        }
    }

    Ok(())
}

// =========================================================================
// Pattern helpers
// =========================================================================

/// Try to invoke an element (click a button, etc.) via its Invoke
/// control pattern.
///
/// # Safety
///
/// Must be called on a COM-initialised thread. `element` must be a
/// valid COM pointer with an `IUIAutomationInvokePattern`.
pub unsafe fn invoke_element(element: &IUIAutomationElement) -> CuaResult<()> {
    let pattern_iunknown = element
        .GetCurrentPattern(UIA_InvokePatternId)
        .map_err(|e| CuaError::Uia(format!("GetCurrentPattern(Invoke): {e}")))?;

    let pattern: IUIAutomationInvokePattern = pattern_iunknown
        .cast()
        .map_err(|e| CuaError::Uia(format!("QI InvokePattern: {e}")))?;

    pattern
        .Invoke()
        .map_err(|e| CuaError::Uia(format!("Invoke: {e}")))?;

    Ok(())
}

/// Get the clickable point of an element as `(x, y)` screen coordinates.
///
/// # Safety
///
/// Must be called on a COM-initialised thread.
pub unsafe fn get_clickable_point(element: &IUIAutomationElement) -> CuaResult<(i32, i32)> {
    let mut pt = POINT { x: 0, y: 0 };
    let _got_clickable = element
        .GetClickablePoint(&mut pt as *mut POINT)
        .map_err(|e| CuaError::Uia(format!("GetClickablePoint: {e}")))?;
    Ok((pt.x, pt.y))
}

/// Set keyboard focus to a UIA element via `IUIAutomationElement::SetFocus`.
///
/// # Safety
///
/// Must be called on a COM-initialised thread.
pub unsafe fn set_focus_element(element: &IUIAutomationElement) -> CuaResult<()> {
    element
        .SetFocus()
        .map_err(|e| CuaError::Uia(format!("SetFocus: {e}")))?;
    Ok(())
}

/// Convenience: find an element by name, then get its clickable point.
/// This bundles `find_first_raw + get_clickable_point` into one call,
/// avoiding the need for the caller to manage raw COM pointers.
///
/// # Safety
///
/// Must be called on a COM-initialised thread.
pub unsafe fn click_by_name(
    automation: &UiaAutomation,
    name: &str,
) -> CuaResult<(i32, i32)> {
    let cond = UiaCondition::Name(name.to_string());
    let raw = automation
        .find_first_raw(&cond)?
        .ok_or_else(|| CuaError::NotFound(format!("no UIA element with name={name:?}")))?;
    get_clickable_point(&raw)
}

/// Convenience: find an element by name and invoke its default action.
///
/// # Safety
///
/// Must be called on a COM-initialised thread.
pub unsafe fn invoke_by_name(automation: &UiaAutomation, name: &str) -> CuaResult<()> {
    let cond = UiaCondition::Name(name.to_string());
    let raw = automation
        .find_first_raw(&cond)?
        .ok_or_else(|| CuaError::NotFound(format!("no UIA element with name={name:?}")))?;
    invoke_element(&raw)
}

/// Convenience: find an element by name and set keyboard focus.
pub unsafe fn focus_by_name(automation: &UiaAutomation, name: &str) -> CuaResult<()> {
    let cond = UiaCondition::Name(name.to_string());
    let raw = automation
        .find_first_raw(&cond)?
        .ok_or_else(|| CuaError::NotFound(format!("no UIA element with name={name:?}")))?;
    set_focus_element(&raw)
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_type_round_trips() {
        let cases = [
            ControlType::Button,
            ControlType::Edit,
            ControlType::Window,
            ControlType::CheckBox,
            ControlType::ListItem,
            ControlType::TreeItem,
            ControlType::Text,
            ControlType::Pane,
            ControlType::Image,
            ControlType::Tab,
            ControlType::Hyperlink,
            ControlType::Unknown(99999),
        ];
        for ct in &cases {
            let id = control_type_to_id(*ct);
            let back = control_type_from_id(id.0);
            match ct {
                ControlType::Unknown(raw) => {
                    assert_eq!(back, ControlType::Unknown(*raw));
                }
                known => assert_eq!(back, *known, "control type mismatch for {known:?}"),
            }
        }
    }

    #[test]
    fn uia_condition_variants_are_constructable() {
        let _c1 = UiaCondition::Name("Notepad".into());
        let _c2 = UiaCondition::AutomationId("123".into());
        let _c3 = UiaCondition::ClassName("Button".into());
        let _c4 = UiaCondition::ControlType(ControlType::Button);
        let _c5 = UiaCondition::And(vec![_c1.clone(), _c2.clone()]);
        let _c6 = UiaCondition::Or(vec![_c3.clone(), _c4.clone()]);
    }

    #[test]
    fn ui_element_is_serializable() {
        let el = UiElement {
            name: "Test".into(),
            class_name: "Button".into(),
            automation_id: "btn1".into(),
            control_type: ControlType::Button,
            rect: ElementRect::default(),
            is_enabled: true,
            is_offscreen: false,
            process_id: 1234,
            has_keyboard_focus: false,
            is_password: false,
            help_text: String::new(),
            item_status: String::new(),
            item_type: String::new(),
            localized_control_type: "button".into(),
            accelerator_key: String::new(),
            access_key: String::new(),
            framework_id: "WPF".into(),
            provider_description: String::new(),
            depth: 0,
            index: 0,
        };
        let json = serde_json::to_string(&el).expect("UiElement should serialize");
        assert!(json.contains("\"name\":\"Test\""));
        assert!(json.contains("\"control_type\":\"button\""));
        assert!(json.contains("\"framework_id\":\"WPF\""));
    }
}
