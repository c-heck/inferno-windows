function Get-AsioBinaryArchitecture {
    param([Parameter(Mandatory = $true)][string]$Path)
    $stream = [IO.File]::OpenRead((Resolve-Path -LiteralPath $Path).Path)
    $reader = New-Object IO.BinaryReader($stream)
    try {
        if ($reader.ReadUInt16() -ne 0x5A4D) { throw 'Not a PE binary.' }
        $stream.Position = 0x3c
        $pe = $reader.ReadUInt32()
        $stream.Position = $pe
        if ($reader.ReadUInt32() -ne 0x4550) { throw 'Invalid PE signature.' }
        $machine = $reader.ReadUInt16()
        $sections = $reader.ReadUInt16()
        $stream.Position = $pe + 20
        $optionalSize = $reader.ReadUInt16()
        $optional = $pe + 24
        $stream.Position = $optional
        if ($reader.ReadUInt16() -ne 0x20b) { throw 'Expected a 64-bit PE binary.' }
        if ($machine -eq 0xAA64) { return 'ARM64' }
        if ($machine -ne 0x8664) { throw ('Unsupported PE machine: 0x{0:X4}' -f $machine) }
        # Linked ARM64EC images report x64; distinguish them using CHPE metadata.
        if ($optionalSize -lt 200) { return 'x64' }
        $stream.Position = $optional + 108
        if ($reader.ReadUInt32() -le 10) { return 'x64' }
        $stream.Position = $optional + 192
        $loadRva = $reader.ReadUInt32()
        $loadSize = $reader.ReadUInt32()
        if ($loadRva -eq 0 -or $loadSize -lt 208) { return 'x64' }
        $loadOffset = $null
        for ($i = 0; $i -lt $sections; $i++) {
            $stream.Position = $optional + $optionalSize + $i * 40 + 8
            $virtualSize = $reader.ReadUInt32()
            $rva = $reader.ReadUInt32()
            $rawSize = $reader.ReadUInt32()
            $rawOffset = $reader.ReadUInt32()
            if ($loadRva -ge $rva -and [uint64]$loadRva + 208 -le [uint64]$rva + $rawSize -and
                $loadRva -lt [uint64]$rva + [Math]::Max($virtualSize, $rawSize)) {
                $loadOffset = [uint64]$rawOffset + $loadRva - $rva
                break
            }
        }
        if ($null -eq $loadOffset -or $loadOffset + 208 -gt $stream.Length) { throw 'Invalid PE load-config location.' }
        $stream.Position = $loadOffset
        if ($reader.ReadUInt32() -lt 208) { return 'x64' }
        $stream.Position = $loadOffset + 200
        if ($reader.ReadUInt64() -eq 0) { return 'x64' }
        return 'ARM64EC'
    } finally { $reader.Dispose() }
}
