run_first_c = function(c, vals)
    for i=1,vals.n do
        local f = vals[i]
        if f == nil then
            error("Argument " .. i .. " of 'run_first' is not defined")
        end

        local res = f(c);
        if res:is_ok() or res:is_error() then
            return res;
        end
    end
    return res_noop();
end

run_first = function(...)
    local vals = table.pack(...)
    return function(c)
        return run_first_c(c, vals)
    end;
end

run_all_c = function(c, vals)
    local res = res_noop();

    for i=1,vals.n do
        local f = vals[i]
        if f == nil then
            error("Argument " .. i .. " of 'run_all' is not defined")
        end

        res = f(c);
        if res:is_error() then
            return res;
        end
    end
    return res;
end

run_all = function(...)
    local vals = table.pack(...)

    return function(c)
        return run_all_c(c, vals)
    end;
end

define_mode('visual', 'normal')
define_mode('insert-line', 'insert')

bind('q', 'normal', quit)
bind('i', 'normal', push_mode("insert-line"))
bind('I', 'normal', push_mode("insert"))
bind('o', 'normal', push_mode("roomfilter"))
bind('O', 'normal', push_mode("roomfilterunread"))
bind(':', 'normal', push_mode("command"))
bind('v', 'normal', run_all(push_mode("visual"), select_prev_message))
bind('<Esc>', 'normal', run_first(clear_error_message, deselect_message, cancel_special_message))
bind('<C-n>', 'normal', select_next_room)
bind('<C-p>', 'normal', select_prev_room)
bind('<C-i>', 'normal', select_room_history_next)
bind('<C-o>', 'normal', select_room_history_prev)
bind('<C-c>', 'normal', clear_message)
bind('h', 'normal', cursor_move_left)
bind('l', 'normal', cursor_move_right)
bind('k', 'normal', cursor_move_up)
bind('j', 'normal', cursor_move_down)
bind('0', 'normal', cursor_move_beginning_of_line)
bind('$', 'normal', cursor_move_end_of_line)
bind('<Return>', 'normal', send_message)

bind('<C-c>', 'insert', clear_message)
bind('<Esc>', 'insert', pop_mode)
bind('<Left>', 'insert', cursor_move_left)
bind('<Right>', 'insert', cursor_move_right)
bind('<Up>', 'insert', cursor_move_up)
bind('<Down>', 'insert', cursor_move_down)
bind('<Backspace>', 'insert', cursor_delete_left)
bind('<Delete>', 'insert', cursor_delete_right)
bind('<Home>', 'insert', cursor_move_beginning_of_line)
bind('<End>', 'insert', cursor_move_end_of_line)

bind('<C-c>', 'insert-line', clear_message)
bind('<Esc>', 'insert-line', pop_mode)
bind('<Left>', 'insert-line', cursor_move_left)
bind('<Right>', 'insert-line', cursor_move_right)
bind('<Backspace>', 'insert-line', cursor_delete_left)
bind('<Delete>', 'insert-line', cursor_delete_right)
bind('<Home>', 'insert-line', cursor_move_beginning_of_line)
bind('<End>', 'insert-line', cursor_move_end_of_line)
bind('<Return>', 'insert-line', send_message)

bind('<C-n>', 'roomfilter', select_next_room)
bind('<C-p>', 'roomfilter', select_prev_room)
bind('<Esc>', 'roomfilter', pop_mode)
bind('<Return>', 'roomfilter', run_all(force_room_selection, pop_mode))

bind('<C-n>', 'roomfilterunread', select_next_room)
bind('<C-p>', 'roomfilterunread', select_prev_room)
bind('<Esc>', 'roomfilterunread', pop_mode)
bind('<Return>', 'roomfilterunread', run_all(force_room_selection, pop_mode))

bind('<Esc>', 'command', run_first(clear_error_message, pop_mode))
bind('<Return>', 'command', run_all(run_command, pop_mode))

bind('k', 'visual', select_prev_message)
bind('j', 'visual', select_next_message)
bind('r', 'visual', run_all(start_reply, deselect_message, switch_mode("insert-line")))
bind('c', 'visual', run_all(start_edit, deselect_message, switch_mode("insert-line")))
bind('<Esc>', 'visual', run_all(deselect_message, pop_mode))
bind(':', 'visual', push_mode("command"))
bind('<Return>', 'visual', open_selected_message)

bind('p', 'normal', function(c)
    content = c:get_clipboard()
    return c:type(content)
end)

bind('y', 'visual', function(c)
    content = c:get_message_content()
    c:set_clipboard(content)
    return res_ok()
end)

e = clear_timeline_cache
q = quit
