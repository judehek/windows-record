# Windows Window Recorder

## Overview
This is a window recorder built using the `windows` crate in Rust. It uses desktop duplication and Windows Media Foundation Transforms and Sinks in order to record a window without any yellow box being drawn around the border of the window. It will black out the recording if you are not focused on the window you want to record.

## Features
- Recording a window with audio (Up to 60fps tested).
- H.264 Codec output supporting MP4 files
- Abstracted interface that leaves all difficult and `unsafe` code away from a user.

## In Progress
- Allowing it to record on different monitors, and enumerating to find the proper monitor
- Convert this to a lib-style repo so that it can be used with cargo and the interfaces can be integrated into a wider project
- Further performance increases by decreasing the required alloc's and potential speedups along the Windows Media Foundation pipeline
- Ability to choose codecs to use for better performance
