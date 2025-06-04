-- Required configuration:
host("my-matrix-server.com")
user("my-user-name")

------------------------------------------
-- Optional exemplary customization below:
------------------------------------------

-- Show only names in notifications (not text)
notification_style("nameonly")

-- Override user colors if desired
user_color_ansi("@my-friend:my-matrix-server.com", 3);

-- Helper to select frequently used rooms
function select_room(room_name)
    return run_all(push_mode("roomfilter"), type(room_name), force_room_selection, pop_mode)
end
bind('gs', 'normal', select_room("Note to self"))
