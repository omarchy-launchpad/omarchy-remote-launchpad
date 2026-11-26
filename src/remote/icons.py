"""Resolve desktop-entry icons on the Linux host and stream bounded PNGs."""
from configparser import ConfigParser, Error
from ctypes import CDLL, POINTER, byref, c_char_p, c_int, c_size_t, c_uint32, c_void_p, string_at
from os import environ, fsdecode, fsencode
from pathlib import Path
import subprocess
import sys


def bind(library, name, result, *arguments):
    function = getattr(library, name)
    function.restype = result
    function.argtypes = arguments
    return function


def import_desktop_paths():
    if environ.get("DISPLAY"):
        return
    try:
        session = subprocess.run(
            ["systemd-run", "--user", "--quiet", "--pipe", "--wait", "/usr/bin/env", "-0"],
            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
            timeout=0.5, check=True,
        )
    except (OSError, subprocess.SubprocessError):
        return
    for field in session.stdout.split(b"\0"):
        key, _, value = field.partition(b"=")
        if key in (b"XDG_DATA_HOME", b"XDG_DATA_DIRS", b"XDG_CONFIG_HOME"):
            environ[fsdecode(key)] = fsdecode(value)


def icon_theme_name():
    config = Path(environ.get("XDG_CONFIG_HOME") or Path.home() / ".config")
    for version in ("3.0", "4.0"):
        settings = ConfigParser(interpolation=None)
        try:
            settings.read(config / f"gtk-{version}" / "settings.ini", encoding="utf-8")
            name = settings.get("Settings", "gtk-icon-theme-name", fallback="").strip()
            if name:
                return name
        except (Error, OSError, UnicodeError):
            pass
    return "Adwaita"


def main():
    ids = sys.stdin.buffer.read(1024 * 1024 + 1)
    if len(ids) > 1024 * 1024:
        return
    import_desktop_paths()
    try:
        gio = CDLL("libgio-2.0.so.0")
        gtk = CDLL("libgtk-3.so.0")
        pixbuf = CDLL("libgdk_pixbuf-2.0.so.0")
    except OSError:
        return

    desktop = bind(gio, "g_desktop_app_info_new", c_void_p, c_char_p)
    desktop_string = bind(gio, "g_desktop_app_info_get_string", c_void_p, c_void_p, c_char_p)
    unref = bind(gio, "g_object_unref", None, c_void_p)
    free = bind(gio, "g_free", None, c_void_p)
    theme_new = bind(gtk, "gtk_icon_theme_new", c_void_p)
    set_theme = bind(gtk, "gtk_icon_theme_set_custom_theme", None, c_void_p, c_char_p)
    lookup = bind(gtk, "gtk_icon_theme_lookup_icon", c_void_p, c_void_p, c_char_p, c_int, c_int)
    filename = bind(gtk, "gtk_icon_info_get_filename", c_char_p, c_void_p)
    load = bind(pixbuf, "gdk_pixbuf_new_from_file_at_scale", c_void_p,
                c_char_p, c_int, c_int, c_int, c_void_p)
    new_image = bind(pixbuf, "gdk_pixbuf_new", c_void_p, c_int, c_int, c_int, c_int, c_int)
    width_of = bind(pixbuf, "gdk_pixbuf_get_width", c_int, c_void_p)
    height_of = bind(pixbuf, "gdk_pixbuf_get_height", c_int, c_void_p)
    fill = bind(pixbuf, "gdk_pixbuf_fill", None, c_void_p, c_uint32)
    copy_area = bind(pixbuf, "gdk_pixbuf_copy_area", None,
                     c_void_p, c_int, c_int, c_int, c_int, c_void_p, c_int, c_int)
    save = bind(pixbuf, "gdk_pixbuf_save_to_bufferv", c_int, c_void_p,
                POINTER(c_void_p), POINTER(c_size_t), c_char_p, c_void_p, c_void_p, c_void_p)

    size = 128
    theme = theme_new()
    set_theme(theme, fsencode(icon_theme_name()))

    def png_for(desktop_id):
        entry = desktop(desktop_id)
        if not entry:
            return None
        value = desktop_string(entry, b"Icon")
        unref(entry)
        if not value:
            return None
        icon = string_at(value)
        free(value)
        if not icon.startswith(b"/"):
            info = lookup(theme, icon, size, 0)
            if not info:
                return None
            icon = filename(info)
            unref(info)
        if not icon:
            return None
        image = load(icon, size, size, 1, None)
        if not image:
            return None
        width, height = width_of(image), height_of(image)
        if width != height:
            square = new_image(0, 1, 8, size, size)
            if not square:
                unref(image)
                return None
            fill(square, 0)
            copy_area(image, 0, 0, width, height, square, (size - width) // 2, (size - height) // 2)
            unref(image)
            image = square
        data, length = c_void_p(), c_size_t()
        written = save(image, byref(data), byref(length), b"png", None, None, None)
        unref(image)
        if not written:
            return None
        result = string_at(data, length.value)
        free(data)
        return result

    remaining = 16 * 1024 * 1024
    for desktop_id in ids.splitlines():
        image = png_for(desktop_id)
        if image and len(image) <= 96 * 1024:
            header = desktop_id + b"\t" + str(len(image)).encode("ascii") + b"\n"
            remaining -= len(header) + len(image)
            if remaining < 0:
                break
            sys.stdout.buffer.write(header)
            sys.stdout.buffer.write(image)
    unref(theme)


if __name__ == "__main__":
    main()
