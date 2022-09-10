__run_first_c = function(c, vals)
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
        return __run_first_c(c, vals)
    end;
end

__run_all_c = function(c, vals)
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
        return __run_all_c(c, vals)
    end;
end

function finish_auxline(run_on_content, run_on_empty)
    return function(c)
        content = c:get_auxline_content()
        res = res_ok()
        if(content ~= "") then
            c:accept_auxline()
            res = run_on_content(c, content)
        end
        c:pop_mode()
        return res
    end
end

function vim_delete(from, to)
    return function(c)
        content = c:cursor_yank(from, to)
        c:set_clipboard(content)
        c:cursor_delete(from, to)
        return res_ok()
    end
end

function vim_change(from, to)
    return function(c)
        content = c:cursor_yank(from, to)
        c:set_clipboard(content)
        c:cursor_delete(from, to)
        return c:push_mode("insert-line")
    end
end

function vim_yank(from, to)
    return function(c)
        content = c:cursor_yank(from, to)
        c:set_clipboard(content)
        return res_ok()
    end
end

function __bind_ydc_normal(sequence, from, to)
    mode = 'normal'
    bind('d' .. sequence, mode, vim_delete(from, to))
    bind('c' .. sequence, mode, vim_change(from, to))
    bind('y' .. sequence, mode, vim_yank(from, to))
end

function __bind_vim_forward_normal(sequence, to)
    __bind_ydc_normal(sequence, 'cursor', to)
    bind(sequence, 'normal', cursor_move_forward(to))
end

function __bind_vim_backward_normal(sequence, from)
    __bind_ydc_normal(sequence, from, 'cursor')
    bind(sequence, 'normal', cursor_move_backward(from))
end


-- normal mode
bind('q', 'normal', quit)
bind('i', 'normal', push_mode("insert-line"))
bind('I', 'normal', push_mode("insert"))
bind('o', 'normal', push_mode("roomfilter"))
bind('O', 'normal', push_mode("roomfilterunread"))
bind(':', 'normal', push_mode("command"))
bind('v', 'normal', run_all(push_mode("visual"), select_prev_message))
bind('L', 'normal', push_mode("limit"))
bind('<Esc>', 'normal', run_first(clear_error_message, deselect_message, cancel_special_message))
bind('<C-n>', 'normal', select_next_room)
bind('<C-p>', 'normal', select_prev_room)
bind('<C-i>', 'normal', select_room_history_next)
bind('<C-o>', 'normal', select_room_history_prev)
bind('<C-c>', 'normal', clear_message)
bind('<Return>', 'normal', send_message)

-- vim-like bindings
bind('k', 'normal', cursor_move_up)
bind('j', 'normal', cursor_move_down)
bind('x', 'normal', cursor_delete_right)
bind('X', 'normal', cursor_delete_left)
bind('a', 'normal', run_all(cursor_move_forward('cell'), push_mode("insert-line")))
bind('A', 'normal', run_all(cursor_move_forward('line_separator'), push_mode("insert-line")))

__bind_vim_forward_normal('l', 'cell')
__bind_vim_forward_normal('$', 'line_separator')
__bind_vim_forward_normal('w', 'word_begin')
__bind_vim_forward_normal('W', 'WORD_begin')
__bind_vim_forward_normal('e', 'word_end')
__bind_vim_forward_normal('E', 'WORD_end')
__bind_vim_forward_normal(')', 'sentence')
__bind_vim_forward_normal('G', 'document_boundary')

__bind_vim_backward_normal('h', 'cell')
__bind_vim_backward_normal('0', 'line_separator')
__bind_vim_backward_normal('b', 'word_begin')
__bind_vim_backward_normal('B', 'WORD_begin')
__bind_vim_backward_normal('(', 'sentence')
__bind_vim_backward_normal('gg', 'document_boundary')

__bind_ydc_normal('iw', 'word_begin', 'word_end')
__bind_ydc_normal('iW', 'WORD_begin', 'WORD_end')
bind('dd', 'normal', vim_delete('line_separator', 'line_separator'))
bind('cc', 'normal', vim_change('line_separator', 'line_separator'))
bind('yy', 'normal', vim_yank('line_separator', 'line_separator'))
bind('D', 'normal', vim_delete('cursor', 'line_separator'))
bind('C', 'normal', vim_change('cursor', 'line_separator'))
bind('Y', 'normal', vim_yank('cursor', 'line_separator'))
bind('P', 'normal', function(c)
    content = c:get_clipboard()
    return c:type(content)
end)
bind('p', 'normal', function(c)
    c:cursor_move_forward('cell')
    content = c:get_clipboard()
    c:cursor_move_backward('cell')
    return c:type(content)
end)

-- insert mode
bind('<C-c>', 'insert', clear_message)
bind('<Esc>', 'insert', run_all(cursor_move_backward('cell'), pop_mode))
bind('<Up>', 'insert', cursor_move_up)
bind('<Down>', 'insert', cursor_move_down)
bind('<Backspace>', 'insert', cursor_delete_left)
bind('<Delete>', 'insert', cursor_delete_right)
bind('<Left>', 'insert', cursor_move_backward('cell'))
bind('<Right>', 'insert', cursor_move_forward('cell'))
bind('<Home>', 'insert', cursor_move_backward('line_separator'))
bind('<End>', 'insert', cursor_move_forward('line_separator'))

-- insert-line mode
define_mode('insert-line', 'insert')
bind('<C-c>', 'insert-line', clear_message)
bind('<Esc>', 'insert-line', run_all(cursor_move_backward('cell'), pop_mode))
bind('<Backspace>', 'insert-line', cursor_delete_left)
bind('<Delete>', 'insert-line', cursor_delete_right)
bind('<Left>', 'insert-line', cursor_move_backward('cell'))
bind('<Right>', 'insert-line', cursor_move_forward('cell'))
bind('<Home>', 'insert-line', cursor_move_backward('line_separator'))
bind('<End>', 'insert-line', cursor_move_forward('line_separator'))
bind('<Return>', 'insert-line', send_message)

-- roomfilter mode
bind('<C-n>', 'roomfilter', select_next_room)
bind('<C-p>', 'roomfilter', select_prev_room)
bind('<Esc>', 'roomfilter', pop_mode)
bind('<Return>', 'roomfilter', run_all(force_room_selection, pop_mode))

-- roomfilterunread mode
bind('<C-n>', 'roomfilterunread', select_next_room)
bind('<C-p>', 'roomfilterunread', select_prev_room)
bind('<Esc>', 'roomfilterunread', pop_mode)
bind('<Return>', 'roomfilterunread', run_all(force_room_selection, pop_mode))

-- command mode
on_enter('command', run_all(switch_auxline('command'), set_auxline_prompt(':')))
bind('<Esc>', 'command', run_first(clear_error_message, run_all(clear_auxline, pop_mode)))
bind('<Return>', 'command', finish_auxline(function(c, content) return c:run(content) end))

-- limit mode
define_mode('limit', 'command')
on_enter('limit', run_all(switch_auxline('limit'), set_auxline_prompt('Limit: ')))
bind('<Esc>', 'limit', run_first(clear_error_message, run_all(clear_auxline, pop_mode)))
bind('<C-c>', 'limit', clear_auxline)
bind('<Return>', 'limit', function(c)
    content = c:get_auxline_content()
    res = res_ok()
    if(content ~= "") then
        c:accept_auxline()
        res = c:set_filter(content)
    else
        c:clear_filter(content)
    end
    c:pop_mode()
    return res
end)

-- send-file mode
define_mode('send-file', 'command')
on_enter('send-file', run_all(switch_auxline('send-file'), set_auxline_prompt('Send file: ')))
bind('<Esc>', 'send-file', run_first(clear_error_message, run_all(clear_auxline, pop_mode)))
bind('<C-c>', 'send-file', clear_auxline)
bind('<Return>', 'send-file', finish_auxline(function(c, content) return c:send_file(content) end))

-- save-file mode
define_mode('save-file', 'command')
on_enter('save-file', run_all(switch_auxline('save-file'), set_auxline_prompt('Save file as: ')))
bind('<Esc>', 'save-file', run_first(clear_error_message, run_all(clear_auxline, pop_mode)))
bind('<C-c>', 'save-file', clear_auxline)
bind('<Return>', 'save-file', finish_auxline(function(c, content) return c:save_file(content) end))

-- react mode
define_mode('react', 'command')
on_enter('react', run_all(switch_auxline('react'), set_auxline_prompt('React with: ')))
bind('<Esc>', 'react', run_first(clear_error_message, run_all(clear_auxline, pop_mode)))
bind('<C-c>', 'react', clear_auxline)
bind('<Return>', 'react', finish_auxline(function(c, content) return c:react(content) end))

-- visual mode
define_mode('visual', 'normal')
bind('k', 'visual', select_prev_message)
bind('j', 'visual', select_next_message)
bind('f', 'visual', follow_reply)
bind('r', 'visual', run_all(start_reply, deselect_message, switch_mode("insert-line")))
bind('R', 'visual', push_mode('react'))
bind('c', 'visual', run_all(start_edit, deselect_message, switch_mode("insert-line")))
bind('s', 'visual', push_mode('save-file'))
bind(':', 'visual', push_mode('command'))
bind('<Esc>', 'visual', run_all(deselect_message, pop_mode))
bind('<Return>', 'visual', open_selected_message)
bind('y', 'visual', function(c)
    content = c:get_message_content()
    c:set_clipboard(content)
    return res_ok()
end)

e = clear_timeline_cache
q = quit
