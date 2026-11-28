$homeDir = 'E:\NeonShim-Exp\vic-home'
& "$PSScriptRoot\scripts\play-foxhound.ps1" `
    -Exe 'D:\victoria\GTA6.Xbox.v1.0.2.1\game_scarlett_master.exe' `
    -Build release -Dlss quality -Preload `
    -More @{
        VICTORY_HOME      = $homeDir
        VICTORY_GAME_DIR  = 'D:\victoria\GTA6.Xbox.v1.0.2.1'
        VICTORY_CACHE_DIR = "$homeDir\cache"
        VICTORY_DATA_DIR  = "$homeDir\data"
    }
