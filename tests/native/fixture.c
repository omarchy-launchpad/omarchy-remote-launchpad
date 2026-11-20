// Disposable application for the AppKit test. A key release changes the pixels
// and opens a second top-level window owned by the same X11 client.
#include <X11/Xlib.h>
int main(void) {
    Display *d = XOpenDisplay(NULL);
    if (!d) return 1;
    Window root = DefaultRootWindow(d);
    Window w = XCreateSimpleWindow(d, root, 0, 0, 96, 64, 0, 0, 0xff0000);
    XStoreName(d, w, "Seams fixture");
    XSelectInput(d, w, KeyPressMask | KeyReleaseMask | ButtonReleaseMask);
    XMapWindow(d, w);
    XSetInputFocus(d, w, RevertToParent, CurrentTime);
    XFlush(d);
    int pressed = 0;
    for (;;) {
        XEvent event;
        XNextEvent(d, &event);
        if (event.type == ButtonPress && event.xbutton.window != w) {
            return 2; // A click on the covered parent reached its dialog.
        }
        if (event.type == ButtonRelease && event.xbutton.x == 275 && event.xbutton.y == 140) {
            XSetWindowBackground(d, w, 0xffff00);
            XClearWindow(d, w);
            XFlush(d);
        }
        if (event.type == KeyPress) pressed = 1;
        if (pressed && event.type == KeyRelease) {
            XSetWindowBackground(d, w, 0x00ff00);
            XClearWindow(d, w);
            Window dialog = XCreateSimpleWindow(d, root, 120, 0, 96, 64, 0, 0, 0x0000ff);
            XSetTransientForHint(d, dialog, w);
            XStoreName(d, dialog, "Seams dialog");
            XSelectInput(d, dialog, ButtonPressMask);
            XMapWindow(d, dialog);
            XSetInputFocus(d, dialog, RevertToParent, CurrentTime);
            XFlush(d);
            pressed = 0;
        }
    }
}
