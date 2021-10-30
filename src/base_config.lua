run_first_c = function(c, functions)
    for _, f in ipairs(functions) do
        local res = f(c);
        if res:is_ok() or res:is_error() then
            return res;
        end
    end
    return res_noop();
end

run_first = function(functions)
    return function(c)
        return run_first_c(c, functions);
    end;
end

run_all_c = function(c, functions)
    local res = res_noop();
    for _, f in ipairs(functions) do
        res = f(c);
        if res:is_error() then
            return res;
        end
    end
    return res;
end

run_all = function(functions)
    return function(c)
        return run_all_c(c, functions);
    end;
end

bind('q', 'normal', quit)
bind('<Return>', 'normal', open_selected_message)
bind('i', 'normal', enter_mode("insert"))
bind('o', 'normal', enter_mode("roomfilter"))
bind('O', 'normal', enter_mode("roomfilterunread"))
bind('k', 'normal', select_prev_message)
bind('j', 'normal', select_next_message)
bind('<Esc>', 'normal', run_first({clear_error_message, deselect_message, cancel_reply}))
bind('G', 'normal', deselect_message)
bind('r', 'normal', run_all({start_reply, enter_insert_mode}))
bind('n', 'normal', select_next_room)
bind('p', 'normal', select_prev_room)
bind('<C-c>', 'normal', clear_message)
bind('h', 'normal', cursor_move_left)
bind('l', 'normal', cursor_move_right)
bind('0', 'normal', cursor_move_beginning_of_line)
bind('$', 'normal', cursor_move_end_of_line)

bind('<C-c>', 'insert', clear_message)
bind('<Esc>', 'insert', enter_mode("normal"))
bind('<Return>', 'insert', send_message)
bind('<Left>', 'insert', cursor_move_left)
bind('<Right>', 'insert', cursor_move_right)
bind('<Backspace>', 'insert', cursor_delete_left)
bind('<Delete>', 'insert', cursor_delete_right)
bind('<Home>', 'insert', cursor_move_beginning_of_line)
bind('<End>', 'insert', cursor_move_end_of_line)

bind('<C-n>', 'roomfilter', select_next_room)
bind('<C-p>', 'roomfilter', select_prev_room)
bind('<Esc>', 'roomfilter', enter_mode("normal"))
bind('<Return>', 'roomfilter', accept_room_selection)

bind('<C-n>', 'roomfilterunread', select_next_room)
bind('<C-p>', 'roomfilterunread', select_prev_room)
bind('<Esc>', 'roomfilterunread', enter_mode("normal"))
bind('<Return>', 'roomfilterunread', accept_room_selection)
