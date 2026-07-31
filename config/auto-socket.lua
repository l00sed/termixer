-- Unified termixer route helper.
-- If TM=1, this script standardizes IPC + PCM paths and
-- lifecycle so the TUI sees one stable SOURCE.

local utils = require "mp.utils"

if os.getenv("TM") ~= "1" then
    return
end

local SOCKET_PATH = "/tmp/termixer.sock"
local FIFO_PATH = "/tmp/termixer.pcm"
local META_PATH = "/tmp/termixer-meta.json"

local pid = tostring(mp.get_property_native("pid") or "")
if pid ~= "" then
    SOCKET_PATH = "/tmp/termixer-" .. pid .. ".sock"
    FIFO_PATH = "/tmp/termixer-" .. pid .. ".pcm"
    META_PATH = "/tmp/termixer-" .. pid .. ".json"
end

local function path_exists(path)
    local st = utils.file_info(path)
    return st ~= nil
end

local function ensure_fifo(path)
    if path_exists(path) then
        return true
    end
    local mk = utils.subprocess({
        args = {"mkfifo", path},
        playback_only = false,
    })
    return mk.status == 0
end

local function remove_if_exists(path)
    if path_exists(path) then
        os.remove(path)
    end
end

local function process_alive(pid_str)
    if not pid_str or pid_str == "" then
        return false
    end
    -- Silence stderr when probing dead PIDs to avoid noisy
    -- "kill: <pid>: No such process" logs in mpv.
    local probe = utils.subprocess({
        args = {"sh", "-c", "kill -0 " .. pid_str .. " >/dev/null 2>&1"},
        playback_only = false,
    })
    return probe.status == 0
end

local function cleanup_orphaned_route_files()
    -- NOTE: mp.utils.readdir("files") only returns regular files and misses
    -- sockets/FIFOs, which are exactly what we need to clean up. Use shell
    -- globbing to include all route artifacts.
    local out = utils.subprocess({
        args = {"sh", "-c", "ls -1 /tmp/termixer-* 2>/dev/null || true"},
        playback_only = false,
    })
    local stdout = out and out.stdout or ""
    for path in stdout:gmatch("[^\n]+") do
        local name = path:match("([^/]+)$") or ""
        local pid_str = name:match("^termixer%-(%d+)%.sock$")
            or name:match("^termixer%-(%d+)%.pcm$")
            or name:match("^termixer%-(%d+)%.json$")
        if pid_str and pid_str ~= pid and not process_alive(pid_str) then
            remove_if_exists(path)
        end
    end

    -- Cleanup legacy non-pid paths from older script versions.
    remove_if_exists("/tmp/termixer.sock")
    remove_if_exists("/tmp/termixer.pcm")
end

local function json_escape(s)
    if not s then
        return ""
    end
    s = tostring(s)
    s = s:gsub("\\", "\\\\")
         :gsub('"', '\\"')
         :gsub("\n", "\\n")
         :gsub("\r", "\\r")
         :gsub("\t", "\\t")
    return s
end

local function get_tag(metadata, keys)
    if type(metadata) ~= "table" then
        return nil
    end
    for _, k in ipairs(keys) do
        local v = metadata[k]
        if type(v) == "string" and v ~= "" then
            return v
        end
    end
    return nil
end

local function write_route_metadata()
    local media_title = mp.get_property("media-title")
    local metadata = mp.get_property_native("metadata") or {}
    local title = get_tag(metadata, {"title", "TITLE"}) or media_title
    local artist = get_tag(metadata, {"artist", "ARTIST"})
    local album = get_tag(metadata, {"album", "ALBUM"})

    local body = string.format(
        '{"media_title":"%s","title":"%s","artist":"%s","album":"%s"}',
        json_escape(media_title),
        json_escape(title),
        json_escape(artist),
        json_escape(album)
    )

    local f = io.open(META_PATH, "w")
    if f then
        f:write(body)
        f:close()
    end
end

local function configure_route_paths()
    remove_if_exists(SOCKET_PATH)
    local ok = mp.set_property("input-ipc-server", SOCKET_PATH)
    if not ok then
        mp.msg.warn("termixer: failed to set input-ipc-server=" .. SOCKET_PATH)
    end

    if ensure_fifo(FIFO_PATH) then
        mp.msg.info("termixer route fifo: " .. FIFO_PATH)
    else
        mp.msg.warn("termixer: failed to ensure fifo " .. FIFO_PATH)
    end

    local ao_file = mp.get_property("options/ao-pcm-file")
    if ao_file ~= FIFO_PATH then
        local set_ok = mp.set_property("options/ao-pcm-file", FIFO_PATH)
        if set_ok then
            mp.msg.info("termixer route ao-pcm-file: " .. FIFO_PATH)
        else
            mp.msg.warn("termixer: failed to set ao-pcm-file=" .. FIFO_PATH .. " (got: " .. tostring(ao_file) .. ")")
        end
    end

    mp.set_property("file-local-options/ao", "pcm")
    mp.set_property("file-local-options/ao-pcm-waveheader", "no")
    mp.set_property("file-local-options/audio-format", "float")
    mp.set_property("file-local-options/audio-channels", "stereo")
end

configure_route_paths()
cleanup_orphaned_route_files()
write_route_metadata()

mp.register_event("file-loaded", write_route_metadata)
mp.register_event("metadata-update", write_route_metadata)
mp.observe_property("media-title", "string", function()
    write_route_metadata()
end)

mp.register_event("shutdown", function()
    remove_if_exists(SOCKET_PATH)
    remove_if_exists(FIFO_PATH)
    remove_if_exists(META_PATH)
end)
