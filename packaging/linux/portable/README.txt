FaderFrame for Linux
====================

Run without installing:

    ./faderframe

This copy is portable: settings, plugin caches, presets and the recordings
of unsaved projects are kept in the "FaderFrame Data" folder next to it, so
the whole folder can live on a USB stick or in your home directory. CLAP
and VST3 plugins placed in "FaderFrame Data/Plug-Ins/CLAP" and
"FaderFrame Data/Plug-Ins/VST3" travel with it (the usual plugin folders
are searched as well). Delete "FaderFrame Data" to use your profile's
folders instead.

Install (menu entry, `faderframe` command, .ffproj files open with it):

    ./install.sh                 for you
    sudo ./install.sh --system   for everyone
    ./install.sh --uninstall     removes it again (--system for everyone's)

Audio: PipeWire is used directly; JACK and ALSA work too
(Preferences → Audio). FaderFrame bundles GTK 4 and its libraries; it needs
a distribution with glibc 2.39 or newer (e.g. Ubuntu 24.04, Fedora 40,
Debian 13, Arch).

FaderFrame is MIT licensed; see LICENSE and THIRD_PARTY_LICENSES.md.
