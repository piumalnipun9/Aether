-- This Source Code Form is subject to the terms of the Mozilla Public
-- License, v. 2.0. If a copy of the MPL was not distributed with this
-- file, You can obtain one at https://mozilla.org/MPL/2.0/.

local log = require("pinnacle.log")
local client = require("pinnacle.grpc.client").client

local set_or_toggle = {
    SET = require("pinnacle.grpc.defs").pinnacle.util.v1.SetOrToggle.SET_OR_TOGGLE_SET,
    [true] = require("pinnacle.grpc.defs").pinnacle.util.v1.SetOrToggle.SET_OR_TOGGLE_SET,
    UNSET = require("pinnacle.grpc.defs").pinnacle.util.v1.SetOrToggle.SET_OR_TOGGLE_UNSET,
    [false] = require("pinnacle.grpc.defs").pinnacle.util.v1.SetOrToggle.SET_OR_TOGGLE_UNSET,
    TOGGLE = require("pinnacle.grpc.defs").pinnacle.util.v1.SetOrToggle.SET_OR_TOGGLE_TOGGLE,
}

---@lcat nodoc
---@class pinnacle.workspace.WorkspaceHandleModule
local workspace_handle = {}

---A workspace handle.
---
---This is a handle that allows manipulation of a workspace.
---
---This can be retrieved through the various `get` functions in the `Workspace` module.
---@class pinnacle.workspace.WorkspaceHandle
---The unique id of this workspace.
---@field id integer
local WorkspaceHandle = {}

---Workspace management.
---
---This module provides utilities for creating and manipulating workspaces.
---
---A workspace is a sort of marker for each of your windows. It allows you to present windows in ways that
---traditional workspaces cannot.
---
---More specifically:
---
--- - A window can have multiple workspaces.
---   - This means that you can have one window show up across multiple "workspaces" if you come something like i3.
--- - An output can display multiple workspaces at once.
---   - This allows you to toggle a workspace and have windows on both workspaces display at once. This is helpful if you, say, want to reference a browser window while coding; you toggle your browser's workspace and temporarily reference it while you work without having to change screens.
---
---If you need to get workspaces beyond the first with the same name, use the `get` method and find what you need.
---@class pinnacle.workspace
---@field private handle pinnacle.workspace.WorkspaceHandleModule
local workspace = {}
workspace.handle = workspace_handle

---Gets all workspaces across all outputs.
---
---@return pinnacle.workspace.WorkspaceHandle[]
function workspace.get_all()
    local response, err = client:pinnacle_workspace_v1_WorkspaceService_Get({})

    if err then
        log.error(err)
        return {}
    end

    assert(response)

    ---@type pinnacle.workspace.WorkspaceHandle[]
    local handles = {}

    for _, id in ipairs(response.workspace_ids or {}) do
        table.insert(handles, workspace_handle.new(id))
    end

    return handles
end

---Gets the workspace with the given name and output.
---
---If `output` is not specified, this uses the focused output.
---
---If an output has more than one workspace with the same name, this returns the first.
---
---#### Example
---```lua
--- -- Get workspaces on the focused output
---local workspace = Workspace.get("Workspace")
---
--- -- Get workspaces on a specific output
---local workspace_on_hdmi1 = Workspace.get("Workspace", Output.get_by_name("HDMI-1"))
---```
---
---@param name string The name of the workspace
---@param output pinnacle.output.OutputHandle? The output to get the workspace on, or `nil` for the focused output.
---
---@return pinnacle.workspace.WorkspaceHandle | nil
function workspace.get(name, output)
    output = output or require("pinnacle.output").get_focused()

    if not output then
        log.error("Cannot get workspaces without output")
        return
    end

    local handles = workspace.get_all()

    ---@type (fun(): { output: pinnacle.output.OutputHandle, name: string })[]
    local requests = {}

    for i, handle in ipairs(handles) do
        requests[i] = function()
            return {
                output = handle:output(),
                name = handle:name(),
            }
        end
    end

    local props = require("pinnacle.util").batch(requests)

    for i, prop in ipairs(props) do
        if prop.output.name == output.name and prop.name == name then
            return handles[i]
        end
    end

    return nil
end

---Adds workspaces with the given names to the specified output.
---
---Returns handles to the created workspaces.
---
---#### Example
---```lua
---local workspaces = Workspace.add(Output.get_by_name("HDMI-1"), "1", "2", "Buckle", "Shoe")
---
--- -- With a table
---local workspace_names = { "1", "2", "Buckle", "Shoe" }
---local workspaces = Workspace.add(Output.get_by_name("HDMI-1"), workspace_names)
---```
---
---@param output pinnacle.output.OutputHandle The output to add workspaces to.
---@param ... string The names of the new workspaces.
---
---@return pinnacle.workspace.WorkspaceHandle[] workspaces Handles to the created workspaces.
---
---@overload fun(output: pinnacle.output.OutputHandle, workspace_names: string[]): pinnacle.workspace.WorkspaceHandle[]
function workspace.add(output, ...)
    local workspace_names = { ... }
    if type(workspace_names[1]) == "table" then
        workspace_names = workspace_names[1] --[=[@as string[]]=]
    end

    local response, err = client:pinnacle_workspace_v1_WorkspaceService_Add({
        output_name = output.name,
        workspace_names = workspace_names,
    })

    if err then
        log.error(err)
        return {}
    end

    assert(response)

    ---@type pinnacle.workspace.WorkspaceHandle[]
    local handles = {}

    for _, id in ipairs(response.workspace_ids or {}) do
        table.insert(handles, workspace_handle.new(id))
    end

    return handles
end

---Removes the given workspaces.
---
---#### Example
---```lua
---local workspaces = Workspace.add(Output.get_by_name("HDMI-1"), "1", "2", "Buckle", "Shoe")
---
---Workspace.remove(workspaces) -- "HDMI-1" no longer has those workspaces
---```
---
---@param workspaces pinnacle.workspace.WorkspaceHandle[] The workspaces to remove.
function workspace.remove(workspaces)
    ---@type integer[]
    local ids = {}

    for _, tg in ipairs(workspaces) do
        table.insert(ids, tg.id)
    end

    local _, err = client:pinnacle_workspace_v1_WorkspaceService_Remove({ workspace_ids = ids })

    if err then
        log.error(err)
    end
end

---@param response pinnacle.workspace.v1.MoveToOutputResponse?
---@return pinnacle.workspace.MoveToOutputError|nil
local function move_to_output_error_from_response(response)
    if not (response and response.error) then
        return nil
    end

    ---@type pinnacle.workspace.MoveToOutputError
    local move_error = {}

    if response.error.output_does_not_exist then
        move_error.output_does_not_exist = true
    end

    if response.error.same_window_on_two_outputs then
        local window_ids = response.error.same_window_on_two_outputs.window_ids or {}
        move_error.same_window_on_two_outputs =
            require("pinnacle.window").handle.new_from_table(window_ids)
    end

    return move_error
end

---Moves existing workspaces to the specified output.
---
---#### Example
---```lua
---local workspace_to_move = Workspace.get("1")
---Workspace.move_to_output(Output.get_by_name("HDMI-1"), { workspace_to_move })
---```
---
---@param output pinnacle.output.OutputHandle The output to move workspaces to.
---@param workspaces pinnacle.workspace.WorkspaceHandle[] The workspaces to move.
---
---@return boolean ok `true` on success.
---@return pinnacle.workspace.MoveToOutputError|nil err Error on failure.
function workspace.move_to_output(output, workspaces)
    ---@type integer[]
    local ids = {}

    for _, tg in ipairs(workspaces) do
        table.insert(ids, tg.id)
    end

    local response, err = client:pinnacle_workspace_v1_WorkspaceService_MoveToOutput({
        output_name = output.name,
        workspace_ids = ids,
    })

    if err then
        log.error(err)
        return false, nil
    end

    local move_error = move_to_output_error_from_response(response)
    if move_error then
        return false, move_error
    end

    return true, nil
end

local signal_name_to_SignalName = {
    active = "WorkspaceActive",
    created = "WorkspaceCreated",
    removed = "WorkspaceRemoved",
}

---@class pinnacle.workspace.WorkspaceSignal Signals related to workspace events.
---@field active fun(workspace: pinnacle.workspace.WorkspaceHandle, active: boolean)? A workspace was set to active or not active.
---@field created fun(workspace: pinnacle.workspace.WorkspaceHandle)? A workspace was created.
---@field removed fun(workspace: pinnacle.workspace.WorkspaceHandle)? A workspace was removed.

---@class pinnacle.workspace.MoveToOutputError
---`true` if the output does not exist.
---@field output_does_not_exist boolean?
---This operation would cause the provided windows to be on multiple outputs.
---@field same_window_on_two_outputs pinnacle.window.WindowHandle[]?

---Connects to a workspace signal.
---
---`signals` is a table containing the signal(s) you want to connect to along with
---a corresponding callback that will be called when the signal is signalled.
---
---This function returns a table of signal handles with each handle stored at the same key used
---to connect to the signal. See `SignalHandles` for more information.
---
---# Example
---```lua
---Workspace.connect_signal({
---    active = function(workspace, active)
---        print("Activity for " .. workspace:name() .. " was set to", active)
---    end
---})
---```
---
---@param signals pinnacle.workspace.WorkspaceSignal The signal you want to connect to
---
---@return pinnacle.signal.SignalHandles signal_handles Handles to every signal you connected to wrapped in a table, with keys being the same as the connected signal.
---
---@see pinnacle.signal.SignalHandles.disconnect_all - To disconnect from these signals
function workspace.connect_signal(signals)
    ---@diagnostic disable-next-line: invisible
    local handles = require("pinnacle.signal").handles.new()

    for signal, callback in pairs(signals) do
        local handle =
            require("pinnacle.signal").add_callback(signal_name_to_SignalName[signal], callback)
        handles[signal] = handle
    end

    return handles
end

--------------------------------------------------------------

---Removes this workspace.
---
---#### Example
---```lua
---local workspaces = Workspace.add(Output.get_by_name("HDMI-1"), "1", "2", "Buckle", "Shoe")
---
---workspaces[2]:remove()
---workspaces[4]:remove()
--- -- "HDMI-1" now only has workspaces "1" and "Buckle"
---```
function WorkspaceHandle:remove()
    workspace.remove({ self })
end

---Activates this workspace and deactivates all other ones on the same output.
---
---#### Example
---```lua
--- -- Assume the focused output has the following inactive workspaces and windows:
--- --  - "1": Alacritty
--- --  - "2": Firefox, Discord
--- --  - "3": Steam
---Workspace.get("2"):switch_to() -- Displays Firefox and Discord
---Workspace.get("3"):switch_to() -- Displays Steam
---```
function WorkspaceHandle:switch_to()
    local _, err = client:pinnacle_workspace_v1_WorkspaceService_SwitchTo({ workspace_id = self.id })

    if err then
        log.error(err)
    end
end

---Sets whether or not this workspace is active.
---
---#### Example
---```lua
--- -- Assume the focused output has the following inactive workspaces and windows:
--- --  - "1": Alacritty
--- --  - "2": Firefox, Discord
--- --  - "3": Steam
---Workspace.get("2"):set_active(true)  -- Displays Firefox and Discord
---Workspace.get("3"):set_active(true)  -- Displays Firefox, Discord, and Steam
---Workspace.get("2"):set_active(false) -- Displays Steam
---```
---
---@param active boolean
function WorkspaceHandle:set_active(active)
    local _, err = client:pinnacle_workspace_v1_WorkspaceService_SetActive({
        workspace_id = self.id,
        set_or_toggle = set_or_toggle[active],
    })

    if err then
        log.error(err)
    end
end

---Toggles this workspace's active state.
---
---#### Example
---```lua
--- -- Assume the focused output has the following inactive workspaces and windows:
--- --  - "1": Alacritty
--- --  - "2": Firefox, Discord
--- --  - "3": Steam
---Workspace.get("2"):toggle_active() -- Displays Firefox and Discord
---Workspace.get("2"):toggle_active() -- Displays nothing
---```
function WorkspaceHandle:toggle_active()
    local _, err = client:pinnacle_workspace_v1_WorkspaceService_SetActive({
        workspace_id = self.id,
        set_or_toggle = set_or_toggle.TOGGLE,
    })

    if err then
        log.error(err)
    end
end

---Moves this workspace to the specified output.
---
---@see pinnacle.workspace.move_to_output - for further information
---@param output pinnacle.output.OutputHandle The output to move this workspace to.
---
---@return boolean ok `true` on success.
---@return pinnacle.workspace.MoveToOutputError|nil err Error on failure.
function WorkspaceHandle:move_to_output(output)
    return workspace.move_to_output(output, { self })
end

---Gets whether or not this workspace is active.
---
---@return boolean
function WorkspaceHandle:active()
    local response, err = client:pinnacle_workspace_v1_WorkspaceService_GetActive({ workspace_id = self.id })

    return response and response.active or false
end

---Gets this workspace's name.
---
---@return string?
function WorkspaceHandle:name()
    local response, err = client:pinnacle_workspace_v1_WorkspaceService_GetName({ workspace_id = self.id })

    if err then
        log.error(err)
    end

    return response and response.name or ""
end

---Gets the output this workspace is on.
---
---@return pinnacle.output.OutputHandle
function WorkspaceHandle:output()
    local response, err = client:pinnacle_workspace_v1_WorkspaceService_GetOutputName({ workspace_id = self.id })

    if err then
        log.error(err)
    end

    local output_name = response and response.output_name or ""
    return require("pinnacle.output").handle.new(output_name)
end

---Gets the windows that have this workspace.
---
---@return pinnacle.window.WindowHandle[]
function WorkspaceHandle:windows()
    local windows = require("pinnacle.window").get_all()

    ---@type (fun(): pinnacle.workspace.WorkspaceHandle[])[]
    local win_workspaces = {}
    for i, window in ipairs(windows) do
        win_workspaces[i] = function()
            return window:workspaces()
        end
    end

    local workspaces = require("pinnacle.util").batch(win_workspaces)
    local wins_on_workspace = {}
    for i, workspaces in ipairs(workspaces) do
        for _, workspace in ipairs(workspaces) do
            if workspace.id == self.id then
                table.insert(wins_on_workspace, windows[i])
                break
            end
        end
    end

    return wins_on_workspace
end

---Convert a WorkspaceHandle to string
---
---@param workspace pinnacle.workspace.WorkspaceHandle
---@return string
local function workspace_tostring(workspace)
    return "workspace{id=" .. workspace.id .. "}"
end

---Creates a new `WorkspaceHandle` from an id.
---@param workspace_id integer
---@return pinnacle.workspace.WorkspaceHandle
function workspace_handle.new(workspace_id)
    ---@type pinnacle.workspace.WorkspaceHandle
    local self = {
        id = workspace_id,
    }
    setmetatable(self, { __index = WorkspaceHandle, __tostring = workspace_tostring })
    return self
end

---@param workspace_ids integer[]
---@return pinnacle.workspace.WorkspaceHandle[]
function workspace_handle.new_from_table(workspace_ids)
    ---@type pinnacle.workspace.WorkspaceHandle[]
    local handles = {}

    for _, id in ipairs(workspace_ids) do
        table.insert(handles, workspace_handle.new(id))
    end

    return handles
end

return workspace
