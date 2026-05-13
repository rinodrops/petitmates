import os.path

# Application bundle
application = defines.get('app', 'build/Petit Mates.app')
appname = os.path.basename(application)

# Contents of the DMG
files = [application]
symlinks = {'Applications': '/Applications'}

# Icon positions (logical points, origin = top-left)
icon_locations = {
    appname:        (190, 180),
    'Applications': (480, 180),
}

# Background image (1320x780 @2x for Retina, logical window 660x390)
background = 'assets/dmg-background.png'

# Finder window appearance
show_status_bar  = False
show_tab_view    = False
show_toolbar     = False
show_pathbar     = False
show_sidebar     = False
sidebar_width    = 180

# Window rect: ((x, y from bottom-left of screen), (width, height))  -- screen coords only
window_rect = ((200, 400), (660, 420))

default_view      = 'icon-view'
show_icon_preview = False
icon_size         = 128
text_size         = 13
