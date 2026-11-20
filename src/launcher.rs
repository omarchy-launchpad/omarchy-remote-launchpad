use crate::{
    config::Host,
    remote::{self, App, Task},
};
use objc2::{
    DefinedClass, MainThreadOnly, define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, ProtocolObject, Sel},
    sel,
};
use objc2_app_kit::*;
use objc2_foundation::{
    MainThreadMarker, NSIndexSet, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};
use std::cell::RefCell;

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}
const WIDTH: f64 = 340.;
const HEIGHT: f64 = 540.;
const INSET: f64 = 20.;
const LIST_WIDTH: f64 = WIDTH - 2. * INSET;
const ROW_HEIGHT: f64 = 40.;

const PANEL: u32 = 0x1d1f29;
const BORDER: u32 = 0x3a3f5b;
const SELECTION: u32 = 0x272936;
const TEXT: u32 = 0xb5bfe2;
const MUTED: u32 = 0x969fc2;
const GREEN: u32 = 0x9dbb61;

fn color(rgb: u32) -> Retained<NSColor> {
    let channel = |shift: u32| ((rgb >> shift) & 255) as f64 / 255.;
    NSColor::colorWithSRGBRed_green_blue_alpha(channel(16), channel(8), channel(0), 1.)
}

fn mono(size: f64) -> Retained<NSFont> {
    NSFont::fontWithName_size(&NSString::from_str("Iosevka"), size)
        .unwrap_or_else(|| NSFont::monospacedSystemFontOfSize_weight(size, 0.))
}

define_class!(
    #[unsafe(super = NSWindow)]
    #[thread_kind = MainThreadOnly]
    struct LaunchpadWindow;
    impl LaunchpadWindow {
        // Borderless windows need to opt in to keyboard input.
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key(&self) -> bool { true }

        #[unsafe(method(cancelOperation:))]
        fn dismiss(&self, _: Option<&AnyObject>) { self.orderOut(None); }
    }
);

define_class!(
    #[unsafe(super = NSTableRowView)]
    #[thread_kind = MainThreadOnly]
    struct AppRow;
    impl AppRow {
        #[unsafe(method(drawSelectionInRect:))]
        fn draw_selection(&self, dirty: NSRect) {
            color(SELECTION).setFill();
            NSRectFill(dirty);
        }

        #[unsafe(method(setSelected:))]
        fn select(&self, selected: bool) {
            unsafe { let _: () = msg_send![super(self), setSelected: selected]; }
            tint_cell(self);
        }
    }
);

fn tint_cell(row: &NSTableRowView) {
    if row.numberOfColumns() > 0
        && let Some(cell) = row
            .viewAtColumn(0)
            .and_then(|v| v.downcast::<NSTableCellView>().ok())
    {
        let selected = row.isSelected();
        // app_row installs both outlets as retained subviews.
        if let Some(label) = unsafe { cell.textField() } {
            label.setTextColor(Some(&color(if selected { GREEN } else { TEXT })));
        }
        if let Some(icon) = unsafe { cell.imageView() } {
            icon.setContentTintColor(Some(&color(if selected { GREEN } else { MUTED })));
        }
    }
}

define_class!(
    #[unsafe(super=NSObject)]
    #[thread_kind=MainThreadOnly]
    #[ivars=RefCell<Vec<App>>]
    struct Rows;
    unsafe impl NSObjectProtocol for Rows {}
    unsafe impl NSTableViewDataSource for Rows {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn count(&self,_:&NSTableView)->isize {self.ivars().borrow().len() as isize}
    }
    unsafe impl NSControlTextEditingDelegate for Rows {}
    unsafe impl NSTableViewDelegate for Rows {
        #[unsafe(method_id(tableView:rowViewForRow:))]
        fn row_view(&self, _: &NSTableView, _: isize) -> Option<Retained<NSTableRowView>> {
            let view: Retained<AppRow> = unsafe { msg_send![AppRow::alloc(self.mtm()), initWithFrame: NSRect::ZERO] };
            Some(view.into_super())
        }
        #[unsafe(method(tableView:didAddRowView:forRow:))]
        fn added_row(&self, _: &NSTableView, row: &NSTableRowView, _: isize) {
            tint_cell(row);
        }
        #[unsafe(method_id(tableView:viewForTableColumn:row:))]
        fn cell(&self, _: &NSTableView, _: Option<&NSTableColumn>, row: isize) -> Option<Retained<NSView>> {
            let apps = self.ivars().borrow();
            apps.get(row as usize).map(|app| app_row(self.mtm(), app))
        }
    }
);
fn app_row(mtm: MainThreadMarker, app: &App) -> Retained<NSView> {
    let cell = NSTableCellView::initWithFrame(
        NSTableCellView::alloc(mtm),
        rect(0., 0., LIST_WIDTH, ROW_HEIGHT),
    );
    let icon = NSImageView::initWithFrame(NSImageView::alloc(mtm), rect(12., 12., 16., 16.));
    let symbol = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str("square.on.square"),
        None,
    );
    icon.setImage(symbol.as_deref());
    let name = NSTextField::labelWithString(&NSString::from_str(&app.name), mtm);
    name.setFont(Some(&mono(14.)));
    name.setUsesSingleLineMode(true);
    name.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
    name.setFrame(rect(38., 10., LIST_WIDTH - 48., 19.));
    name.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
    cell.addSubview(&icon);
    cell.addSubview(&name);
    // Both weak outlets are retained by the cell's subview hierarchy.
    unsafe {
        cell.setImageView(Some(&icon));
        cell.setTextField(Some(&name));
    }
    cell.setToolTip(Some(&NSString::from_str(&format!(
        "{}\n{}",
        app.name, app.id
    ))));
    cell.into_super()
}

fn button(
    mtm: MainThreadMarker,
    title: &str,
    target: &AnyObject,
    action: Sel,
    frame: NSRect,
) -> Retained<NSButton> {
    let button = unsafe {
        NSButton::buttonWithTitle_target_action(
            &NSString::from_str(title),
            Some(target),
            Some(action),
            mtm,
        )
    };
    button.setBordered(false);
    button.setFont(Some(&mono(12.)));
    button.setContentTintColor(Some(&color(MUTED)));
    button.setFrame(frame);
    button
}

pub struct Launcher {
    pub window: Retained<NSWindow>,
    pub host: Option<Host>,
    open: Retained<NSButton>,
    host_label: Retained<NSTextField>,
    pub(crate) search: Retained<NSTextField>,
    table: Retained<NSTableView>,
    rows: Retained<Rows>,
    status: Retained<NSTextField>,
    task: Option<Task<Vec<App>>>,
    apps: Vec<App>,
    query: String,
}
impl Launcher {
    pub fn new(mtm: MainThreadMarker, target: &AnyObject) -> Self {
        let frame = rect(0., 0., WIDTH, HEIGHT);
        let window: Retained<LaunchpadWindow> = unsafe {
            msg_send![LaunchpadWindow::alloc(mtm), initWithContentRect: frame,
                styleMask: NSWindowStyleMask::Borderless, backing: NSBackingStoreType::Buffered, defer: false]
        };
        let window = window.into_super();
        unsafe {
            window.setReleasedWhenClosed(false);
        }
        window.setTitle(&NSString::from_str("Seams Launchpad"));
        window.setAppearance(
            NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua }).as_deref(),
        );
        window.setBackgroundColor(Some(&color(PANEL)));
        window.setHasShadow(true);
        window.setMovableByWindowBackground(true);
        window.center();

        let v = window.contentView().unwrap();
        let border = NSBox::initWithFrame(NSBox::alloc(mtm), frame);
        border.setBoxType(NSBoxType::Custom);
        border.setTitlePosition(NSTitlePosition::NoTitle);
        border.setCornerRadius(0.);
        border.setBorderWidth(2.);
        border.setBorderColor(&color(BORDER));
        border.setFillColor(&color(PANEL));
        v.addSubview(&border);

        let search =
            NSTextField::initWithFrame(NSTextField::alloc(mtm), rect(INSET - 2., 491., 145., 22.));
        search.setBezeled(false);
        search.setDrawsBackground(false);
        search.setFocusRingType(NSFocusRingType::None);
        search.setFont(Some(&mono(14.)));
        search.setTextColor(Some(&color(TEXT)));
        search.setPlaceholderString(Some(&NSString::from_str("Go…")));
        search.setToolTip(Some(&NSString::from_str(
            "Search by app name or desktop ID",
        )));
        let host_label = NSTextField::labelWithString(&NSString::from_str(""), mtm);
        host_label.setFont(Some(&mono(12.)));
        host_label.setAlignment(NSTextAlignment::Right);
        host_label.setTextColor(Some(&color(MUTED)));
        host_label.setLineBreakMode(NSLineBreakMode::ByTruncatingMiddle);
        host_label.setFrame(rect(167., 492., 123., 19.));
        v.addSubview(&host_label);
        let rows: Retained<Rows> =
            unsafe { msg_send![super(Rows::alloc(mtm).set_ivars(RefCell::default())), init] };
        let table =
            NSTableView::initWithFrame(NSTableView::alloc(mtm), rect(0., 0., LIST_WIDTH, 420.));
        let column = NSTableColumn::initWithIdentifier(
            NSTableColumn::alloc(mtm),
            &NSString::from_str("app"),
        );
        column.setWidth(LIST_WIDTH);
        column.setEditable(false);
        table.addTableColumn(&column);
        table.setStyle(NSTableViewStyle::Plain);
        table.setUsesAutomaticRowHeights(false);
        table.setBackgroundColor(&color(PANEL));
        table.setIntercellSpacing(NSSize::new(0., 2.));
        table.setHeaderView(None);
        table.setRowHeight(ROW_HEIGHT);
        unsafe {
            table.setDataSource(Some(ProtocolObject::from_ref(&*rows)));
            table.setDelegate(Some(ProtocolObject::from_ref(&*rows)));
            table.setTarget(Some(target));
            table.setDoubleAction(Some(sel!(chooseApp:)));
        }
        let scroll = NSScrollView::initWithFrame(
            NSScrollView::alloc(mtm),
            rect(INSET, 60., LIST_WIDTH, 420.),
        );
        scroll.setBackgroundColor(&color(PANEL));
        scroll.setDrawsBackground(true);
        scroll.setHasVerticalScroller(true);
        scroll.setAutohidesScrollers(true);
        scroll.setScrollerStyle(NSScrollerStyle::Overlay);
        scroll.setDocumentView(Some(&table));
        let status = NSTextField::wrappingLabelWithString(
            &NSString::from_str("Reading installed applications over SSH…"),
            mtm,
        );
        status.setFont(Some(&mono(12.)));
        status.setTextColor(Some(&color(MUTED)));
        status.setFrame(rect(INSET - 2., 7., 169., 35.));
        let open = button(
            mtm,
            "Launch",
            target,
            sel!(chooseApp:),
            rect(268., 18., 52., 28.),
        );
        open.setKeyEquivalent(&NSString::from_str("\r"));
        let refresh = button(
            mtm,
            "Refresh",
            target,
            sel!(refresh:),
            rect(200., 18., 60., 28.),
        );
        refresh.setToolTip(Some(&NSString::from_str("Refresh apps")));
        let close = button(
            mtm,
            "×",
            &window,
            sel!(orderOut:),
            rect(294., 488., 28., 28.),
        );
        close.setFont(Some(&mono(16.)));
        close.setKeyEquivalent(&NSString::from_str("\u{1b}"));
        close.setToolTip(Some(&NSString::from_str("Close launchpad")));
        v.addSubview(&close);
        v.addSubview(&refresh);
        open.setEnabled(false);
        v.addSubview(&search);
        v.addSubview(&scroll);
        v.addSubview(&status);
        v.addSubview(&open);
        window.makeFirstResponder(Some(&search));
        window.makeKeyAndOrderFront(None);
        #[allow(deprecated)]
        NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
        Self {
            window,
            host: None,
            host_label,
            open,
            search,
            table,
            rows,
            status,
            task: None,
            apps: Vec::new(),
            query: String::new(),
        }
    }
    pub fn refresh(&mut self, host: Result<Host, String>) {
        self.task = None;
        self.host = None;
        self.apps.clear();
        self.filter();
        self.host_label.setStringValue(&NSString::from_str(""));
        self.host_label.setToolTip(None);
        match host.and_then(|h| {
            self.host_label
                .setStringValue(&NSString::from_str(h.name()));
            self.host_label
                .setToolTip(Some(&NSString::from_str(h.name())));
            let task = remote::catalogue(&h)?;
            self.host = Some(h);
            Ok(task)
        }) {
            Ok(task) => {
                self.task = Some(task);
                self.set_status("Reading installed apps…");
            }
            Err(e) => self.error(&e),
        }
    }
    pub fn present(&self) {
        self.window.makeKeyAndOrderFront(None);
        self.window.makeFirstResponder(Some(&self.search));
        #[allow(deprecated)]
        NSApplication::sharedApplication(self.window.mtm()).activateIgnoringOtherApps(true);
    }
    pub fn move_selection(&mut self, delta: isize) {
        self.tick();
        let count = self.table.numberOfRows();
        if count > 0 {
            let row = (self.table.selectedRow() + delta).clamp(0, count - 1);
            self.table.selectRowIndexes_byExtendingSelection(
                &NSIndexSet::indexSetWithIndex(row as usize),
                false,
            );
            self.table.scrollRowToVisible(row);
        }
    }
    pub fn tick(&mut self) {
        if let Some(result) = self.task.as_ref().and_then(Task::poll) {
            self.task = None;
            match result {
                Ok(apps) => {
                    self.apps = apps;
                    self.filter()
                }
                Err(e) => self.error(&e),
            }
        } else if !self.apps.is_empty() && self.query != self.search.stringValue().to_string() {
            self.filter()
        }
    }
    fn filter(&mut self) {
        self.query = self.search.stringValue().to_string();
        let needle = self.query.trim().to_lowercase();
        let matches: Vec<_> = self
            .apps
            .iter()
            .filter(|a| {
                a.name.to_lowercase().contains(&needle) || a.id.to_lowercase().contains(&needle)
            })
            .cloned()
            .collect();
        let count = matches.len();
        *self.rows.ivars().borrow_mut() = matches;
        self.table.reloadData();
        if count > 0 {
            self.table
                .selectRowIndexes_byExtendingSelection(&NSIndexSet::indexSetWithIndex(0), false);
        }
        self.open.setEnabled(count > 0);
        let message = match (count, needle.is_empty()) {
            (0, true) => "No desktop apps found on this host.".into(),
            (0, false) => "No matching apps. Try another search.".into(),
            (1, _) => "1 application".into(),
            _ => format!("{count} applications"),
        };
        self.set_status(&message);
    }
    pub fn selection(&mut self) -> Option<App> {
        self.tick();
        self.rows
            .ivars()
            .borrow()
            .get(self.table.selectedRow() as usize)
            .cloned()
    }
    pub fn error(&self, message: &str) {
        eprintln!("Seams: {message}");
        self.set_status(message);
    }

    pub fn set_status(&self, e: &str) {
        self.status.setStringValue(&NSString::from_str(e));
        self.status.setToolTip(Some(&NSString::from_str(e)))
    }
}
impl Drop for Launcher {
    fn drop(&mut self) {
        self.window.orderOut(None);
    }
}
