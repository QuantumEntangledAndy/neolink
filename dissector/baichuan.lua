-- This is a Wireshark dissector for the Baichuan/Reolink proprietary IP camera protocol.
-- Copy/symlink it into ~/.local/lib/wireshark/plugins/ and restart Wireshark; it should
-- automatically attempt to decode TCP connections on port 9000.

-- Copyright (c) the Neolink contributors
--
-- This program is free software: you can redistribute it and/or modify it
-- under the terms of the GNU Affero General Public License, version 3, as
-- published by the Free Software Foundation.
--
-- This program is distributed in the hope that it will be useful, but WITHOUT
-- ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or
-- FITNESS FOR A PARTICULAR PURPOSE. See the GNU General Public License for
-- more details.
--
-- You should have received a copy of the GNU General Public License along with
-- this program. If not, see <https://www.gnu.org/licenses/>.

local bc_protocol = Proto("Baichuan",  "Baichuan/Reolink IP Camera Protocol")

local magic_bytes = ProtoField.int32("baichuan.magic", "magic", base.DEC)
local message_id =  ProtoField.int32("baichuan.msg_id", "messageId", base.DEC)
local message_understood  = ProtoField.int32("baichuan.msg_understood", "messageUnderstood", base.DEC)
local message_len = ProtoField.int32("baichuan.msg_len", "messageLen", base.DEC)
local xml_enc_offset = ProtoField.int8("baichuan.xml_encryption_offset", "xmlEncryptionOffset", base.DEC)
local encrypt_xml = ProtoField.bool("baichuan.encrypt_xml", "encrypt_xml", base.NONE)
local channel_id =  ProtoField.int8("baichuan.channel_id", "channel_id", base.DEC)
local stream_id = ProtoField.int8("baichuan.stream_id", "streamID", base.DEC)
local unknown = ProtoField.int8("baichuan.unknown", "unknown", base.DEC)
local msg_handle = ProtoField.int8("baichuan.message_handle", "messageHandle", base.DEC)
local status_code = ProtoField.int16("baichuan.status_code", "status_code", base.DEC)
local message_class = ProtoField.int32("baichuan.msg_class", "messageClass", base.DEC)
local f_bin_offset = ProtoField.int32("baichuan.bin_offset", "binOffset", base.DEC)
local username = ProtoField.string("baichuan.username", "username", base.ASCII)
local password = ProtoField.string("baichuan.password", "password", base.ASCII)

-- UDP Related content
local udp_magic = ProtoField.int32("baichuan.udp_magic", "udp_magic", base.DEC)
local udp_type = ProtoField.int8("baichuan.udp_type", "udp_type", base.DEC)
local udp_message_id = ProtoField.int8("baichuan.udp_message_id", "udp_message_id", base.DEC)
local udp_connection_id = ProtoField.int32("baichuan.udp_connection_id", "udp_connection_id", base.DEC)
local udp_unknown = ProtoField.int32("baichuan.udp_unknown", "udp_unknown", base.DEC)
local udp_tid = ProtoField.int32("baichuan.udp_tid", "udp_tid", base.DEC)
local udp_checksum = ProtoField.int32("baichuan.udp_checksum", "udp_checksum", base.DEC)
local udp_packet_count = ProtoField.int32("baichuan.udp_packet_count", "udp_packet_count", base.DEC)
local udp_last_ack_packet = ProtoField.int32("baichuan.udp_last_ack_packet", "udp_last_ack_packet", base.DEC)
local udp_ack_payload_size = ProtoField.int32("baichuan.udp_ack_payload_size", "ack_payload_size", base.DEC)
local udp_size = ProtoField.int32("baichuan.udp_size", "udp_size", base.DEC)
local report_subcmd = ProtoField.uint32("baichuan.report_subcmd", "Report sub-command", base.HEX)

bc_protocol.fields = {
  magic_bytes,
  message_id,
  message_len,
  message_understood,
  xml_enc_offset,
  channel_id,
  stream_id,
  unknown,
  msg_handle,
  encrypt_xml,
  status_code,
  message_class,
  f_bin_offset,
  username,
  password,
  udp_magic,
  udp_type,
  udp_message_id,
  udp_connection_id,
  udp_unknown,
  udp_tid,
  udp_checksum,
  udp_packet_count,
  udp_last_ack_packet,
  udp_ack_payload_size,
  udp_size,
  report_subcmd,
}

-- Message IDs from libBCSDKWrapper SO handleResponseV20 + notes (baichuan-bcsdk-reverse-engineering.md)
local message_types = {
  [0]="Device report (container)",  -- SO: handleResponseV20 case 0/0x21 → handleDeviceReportCmds
  [1]="login", -- <Encryption> <LoginUser>/<LoginNet> <DeviceInfo>/<StreamInfoList>
  [2]="logout",
  [3]="<Preview> (video)",
  [4]="<Preview> (stop)",
  [5]="<FileInfoList> (replay)",
  [7]="<FileInfoList> (stop)",
  [8]="<FileInfoList> (DL Video)",
  [10]="<TalkAbility>",
  [13]="<FileInfoList> (download)",
  [14]="<FileInfoList>",
  [15]="<FileInfoList>",
  [16]="<FileInfoList>",
  [18]="<PtzControl>",
  [23]="Reboot",
  [25]="<VideoInput> (write)",
  [26]="<VideoInput>", -- <InputAdvanceCfg>
  [31]="Start Motion Alarm",
  [33]="<AlarmEventList>",
  [36]="<ServerPort> (write)",
  [37]="<ServerPort>", -- <HttpPort>/<RtspPort>/<OnvifPort>/<HttpsPort>/<RtmpPort>
  [38]="<Ntp>",
  [39]="<Ntp> (write)",
  [40]="<Ddns>",
  [41]="<Ddns> (write)",
  [42]="<Email>",
  [43]="<Email> (write)",
  [44]="<OsdChannelName>", -- <OsdDatetime>
  [45]="<OsdChannelName> (write)",
  [46]="<MD>",
  [47]="<MD> (write)",
  [50]="<VideoLoss>",
  [51]="<VideoLoss> (write)",
  [52]="<Shelter> (priv mask)",
  [53]="<Shelter> (write)",
  [54]="<RecordCfg>",
  [55]="<RecordCfg> (write)",
  [56]="<Compression>",
  [57]="<Compression> (write)",
  [58]="<AbilitySupport>",  -- <UserList>
  [59]="<UserList> (write)",
  [65]="<ConfigFileInfo> (Export)",
  [66]="<ConfigFileInfo> (Import)",
  [67]="<ConfigFileInfo> (FW Upgrade)",  -- SO: 0x43 upgrade recv
  [68]="<Ftp>",
  [69]="<Ftp> (write)",
  [70]="<FtpTask>",
  [71]="<FtpTask> (write)",
  [76]="<Ip>", -- <Dhcp>/<AutoDNS>/<Dns>
  [77]="<Ip> (write)",
  [78]="<VideoInput> (IPC desc)",  -- SO: 0x4e → device report path
  [79]="<Serial> (ptz)",  -- SO: 0x4f → device report path
  [80]="<VersionInfo>",
  [81]="<Record> (schedule)",
  [82]="<Record> (write)",
  [83]="<HandleException>",
  [84]="<HandleException> (write)",
  [91]="<DisplayOutput>",
  [92]="<DisplayOutput> (write)",
  [93]="<LinkType>",
  [97]="<Upnp>",
  [98]="<Upnp> (write)",
  [99]="<Restore> (factory default)",
  [100]="<AutoReboot> (write)",
  [101]="<AutoReboot>",
  [102]="<HDDInfoList>",
  [103]="<HddInitList> (format)",
  [104]="<SystemGeneral>",
  [105]="<SystemGeneral> (write)",
  [106]="<Dst>",
  [107]="<Dst> (write)",
  [108]="<ConfigFileInfo> (log)",
  [109]="<Snap>",
  [114]="<Uid>",
  [115]="<WifiSignal>",
  [116]="<Wifi>",
  [120]="<OnlineUserList>",
  [122]="<PerformanceInfo>",
  [123]="<ReplaySeek>",  -- SO: 0x7b; notes: ReplaySeek report 0x123
  [124]="<PushInfo>",
  [132]="<VideoInput>", -- <InputAdvanceCfg>
  [133]="<RfAlarm>",
  [141]="<Email> (test)",
  [142]="<DayRecords>",  -- SO: 0x8e BaichuanReplayer 0x836 handler
  [143]="<FileInfoList> (download video)",  -- SO: 0x8f BaichuanDownloader
  [144]="<FileInfoList?>",  -- PCAP: seen in capture; notes: HddInit 0x90
  [145]="<ChannelInfoList>",  -- SO: 0x91 → device report path
  [146]="<StreamInfoList>",
  [151]="<AbilityInfo>",
  [190]="PTZ Preset",
  [192]="<unknown>",  -- 0xc0: RE ambiguous (size/offset in SO)
  [194]="<Ftp> (test)",
  [195]="<AutoUpdate>",
  [199]="<Support>",
  [202]="No-op (0xca)",  -- SO: handleResponseV20 early exit
  [208]="<LedState>",
  [209]="<LedState> (write)",
  [210]="<PTOP>",
  [211]="<PTOP> (write)",
  [212]="<rfAlarmCfg>",
  [213]="<rfAlarmCfg> (write)",
  [216]="<EmailTask> (write)",
  [217]="<EmailTask>",
  [218]="<PushTask> (write)",
  [219]="<PushTask>",
  [228]="<Crop>",
  [229]="<Crop> (write)",
  [230]="<cropSnap>",  -- SO: 0xe6 BaichuanDownloader::handleXMLDataResponse
  [232]="<AudioTask>",
  [234]="UDP Keep Alive",  -- SO: 0xea also device report path
  [241]="Device report (0xf1)",  -- SO: handleDeviceReportCmds
  [242]="Device report (0xf2)",
  [252]="<BatteryInfoList>",  -- SO: 0xfc device report
  [253]="<BatteryInfo>",
  [255]="Device report (0xff)",
  [263]="<audioPlayInfo>",
  [268]="<CloudBindInfo>",
  [281]="<BindNasInfoList>",
  [282]="<CloudLoginKey>",
  [287]="<TimeCfg>",
  [272]="<findAlarmVideo>",
  [273]="<alarmVideoInfo>",
  [274]="<findAlarmVideo>",
  [291]="<FloodlightStatusList>",  -- SO: 0x123 device report (ReplaySeek report)
  [294]="<StartZoomFocus> (read)",
  [295]="<StartZoomFocus> (write)",
  [298]="<Replay/playback stream?>",  -- 0x12a: preview/replay binary
  [299]="<AiCfg>",
  [319]="<timelapseCfg>",
  [342]="<AiDetectCfg>",
  [357]="<Downloader> (0x165)",  -- SO: BaichuanDownloader::handleXMLDataResponse
  [362]="<Downloader> (0x16a)",
  [380]="Device report (0x17c)",  -- SO: handleDeviceReportCmds
  [381]="Replay XML response (0x17d)",  -- SO: BaichuanReplayer::handleXMLDataResponse
  [382]="Replay close stream V2 (0x17e)",  -- SO: BaichuanReplayer::playbackStreamCloseV2
  [398]="Device report (0x18e)",
  [399]="Device report (0x18f)",
  [407]="Device report (0x197)",
  [408]="Device report (0x198)",
  [410]="Device report (0x19a)",
  [412]="Device report (0x19c)",
  [421]="Device report (0x1a5)",
  [429]="Device report (0x1ad)",
  [438]="Device report (0x1b6)",
  [457]="Device report (0x1c9)",
  [464]="Device report (0x1d0)",
  [471]="Device report (0x1d7)",
  [484]="Device report response container (0x1e4)",  -- SO: triggers handleDeviceReportCmds when handle==0
  [490]="Device report (0x1ea)",
  [535]="<Downloader> (0x217)",
  [542]="Device report (0x21e)",
  [547]="Device report (0x223)",
  [573]="Device report (0x23d)",
  [580]="Device report (0x244)",
  [588]="Device report (0x24c)",
  [593]="Device report (0x251)",
  [600]="Device report (0x258)",
  [607]="Device report (0x25f)",
  [623]="Device report (0x26f)",  -- SO: handleDeviceReportCmds template 0xce7
  [634]="Device report (0x27a)",
  [640]="Device report (0x280)",
  [646]="<Downloader> (0x286)",
  [653]="<Downloader> (0x28d)",
  [654]="Device report (0x28e)",
  [657]="Device report (0x291)",
  [663]="Device report (0x297)",
  [668]="<Downloader> (0x29c)",
  [678]="Device report (0x2a6)",
  [693]="Device report (0x2b5)",
  [723]="Device report (0x2d3)",
  [736]="Device report (0x2e0)",
  [753]="Device report (0x2f1)",
}

-- Device report message IDs (handleDeviceReportCmds path). Sub-command at body offset 4; see notes/device-report-message-ids.md
local device_report_msg_ids = {
  [0]=true, [33]=true, [78]=true, [79]=true, [145]=true, [234]=true, [241]=true, [242]=true,
  [252]=true, [255]=true, [291]=true, [380]=true, [398]=true, [399]=true, [407]=true, [438]=true,
  [457]=true, [464]=true, [471]=true, [484]=true, [490]=true, [542]=true, [547]=true, [573]=true,
  [580]=true, [588]=true, [600]=true, [623]=true, [634]=true, [640]=true, [654]=true, [657]=true,
  [663]=true, [678]=true, [693]=true, [723]=true, [736]=true, [753]=true,
}
-- Report sub-command (at body+4) → human-readable label (from handleDeviceReportCmds + Neolink model.rs)
local report_subcmd_names = {
  [33]="AlarmEventList / motion", [145]="ChannelInfoList", [234]="UDP keepalive (special)",
  [241]="Report 0xf1 (template 0x890)", [242]="Report 0xf2 (template 0x891)", [252]="BatteryInfoList",
  [255]="Report 0xff (template 0x89e)", [291]="ReplaySeek report", [380]="Report 0x17c (template 0x90d)",
  [398]="Report 0x18e (template 0x91b)", [399]="Report 0x18f (template 0x91c)", [407]="Report 0x197 (template 0x91f)",
  [438]="Floodlight tasks (FloodlightTasksRead)", [457]="Report 0x1c9 (template 0x951)", [464]="Report 0x1d0 (template 0x952)",
  [471]="Report 0x1d7 (template 0x953)", [484]="Report 0x1e4 (template 0x96d)", [490]="Report 0x1ea (template 0x965)",
  [542]="Report 0x21e (template 0x996)", [547]="Report 0x223 (template 0x999)", [573]="Report 0x23d (template 0x9ac)",
  [580]="Report 0x244 (template 0x9d3)", [588]="Report 0x24c (template 0x9b2)", [600]="Report 0x258 (template 0x9bc)",
  [623]="Device report 0x26f (template 0x9d2/0xce7)", [640]="Report 0x280 (template 0x9e6)", [654]="Report 0x28e (template 0x9ea)",
  [657]="Report 0x291 (template 0x9ed)", [663]="Report 0x297 (template 0x9f3)", [678]="Report 0x2a6 (template 0xa05)",
  [693]="Report 0x2b5 (template 0xa0f, simpleRsp)", [723]="Report 0x2d3 (template 0xa19)", [736]="Report 0x2e0 (template 0xa22)",
  [753]="Report 0x2f1 (template 0xa37)",
}

local message_classes = {
  [0x6514]="legacy",
  [0x6614]="modern",
  [0x6414]="modern",
  [0x6482]="modern (file download)",
  [0x0000]="modern",
}

local header_lengths = {
  [0x6514]=20,
  [0x6614]=20,
  [0x6414]=24,
  [0x6482]=24,
  [0x0000]=24,
}

-----
-- Decryption routine.
-----
-- AES decryption uses either Wireshark's built-in GcryptCipher (no extra install) or
-- optional luagcrypt. Without either, AES decryption is skipped (headers and XOR/xml_decrypt still work).
bc_protocol.prefs.key = Pref.string("Decryption key", "",
    "Passphrase used for the camera. Required to decrypt the AES packets")
_G.nonce = {}
local function hexencode(str)
     return (str:gsub(".", function(char) return string.format("%02X", char:byte()) end))
end

-- Minimal pure-Lua MD5 (bit32) for key derivation when using Wireshark's GcryptCipher (no luagcrypt needed).
local function md5_binary(msg)
  local band, bor, bxor, bnot, rshift = bit32.band, bit32.bor, bit32.bxor, bit32.bnot, bit32.rshift
  local lshift = bit32.lshift
  -- Rotate left by n bits (32-bit): (x << n) | (x >> (32 - n))
  local function lrotate(x, n)
    n = n % 32
    return bor(band(lshift(x, n), 0xFFFFFFFF), rshift(x, 32 - n))
  end
  local function F(x,y,z) return bor(band(x,y), band(bnot(x),z)) end
  local function G(x,y,z) return bor(band(x,z), band(y,bnot(z))) end
  local function H(x,y,z) return bxor(x,bxor(y,z)) end
  local function I(x,y,z) return bxor(y, bor(x, bnot(z))) end
  local function bytes2word(b0,b1,b2,b3)
    return bor(bor(bor(b0, lshift(b1,8)), lshift(b2,16)), lshift(b3,24))
  end
  local S = {
    7,12,17,22,7,12,17,22,7,12,17,22,7,12,17,22,
    5,9,14,20,5,9,14,20,5,9,14,20,5,9,14,20,
    4,11,16,23,4,11,16,23,4,11,16,23,4,11,16,23,
    6,10,15,21,6,10,15,21,6,10,15,21,6,10,15,21
  }
  local K = {
    0xd76aa478,0xe8c7b756,0x242070db,0xc1bdceee,0xf57c0faf,0x4787c62a,0xa8304613,0xfd469501,
    0x698098d8,0x8b44f7af,0xffff5bb1,0x895cd7be,0x6b901122,0xfd987193,0xa679438e,0x49b40821,
    0xf61e2562,0xc040b340,0x265e5a51,0xe9b6c7aa,0xd62f105d,0x02441453,0xd8a1e681,0xe7d3fbc8,
    0x21e1cde6,0xc33707d6,0xf4d50d87,0x455a14ed,0xa9e3e905,0xfcefa3f8,0x676f02d9,0x8d2a4c8a,
    0xfffa3942,0x8771f681,0x6d9d6122,0xfde5380c,0xa4beea44,0x4bdecfa9,0xf6bb4b60,0xbebfbc70,
    0x289b7ec6,0xeaa127fa,0xd4ef3085,0x04881d05,0xd9d4d039,0xe6db99e5,0x1fa27cf8,0xc4ac5665,
    0xf4292244,0x432aff97,0xab9423a7,0xfc93a039,0x655b59c3,0x8f0ccc92,0xffeff47d,0x85845dd1,
    0x6fa87e4f,0xfe2ce6e0,0xa3014314,0x4e0811a1,0xf7537e82,0xbd3af235,0x2ad7d2bb,0xeb86d391
  }
  local len = #msg
  local buf = {}
  for i = 1, len do buf[i] = msg:byte(i) end
  buf[len + 1] = 0x80
  local pad = (56 - ((len + 1) % 64) + 64) % 64
  for _ = 1, pad do buf[#buf + 1] = 0 end
  local lo = band(len * 8, 0xFFFFFFFF)
  local hi = math.floor(len * 8 / 0x100000000)
  for _, v in ipairs({ band(lo, 0xFF), band(rshift(lo, 8), 0xFF), band(rshift(lo, 16), 0xFF), band(rshift(lo, 24), 0xFF), band(hi, 0xFF), band(rshift(hi, 8), 0xFF), band(rshift(hi, 16), 0xFF), band(rshift(hi, 24), 0xFF) }) do buf[#buf + 1] = v end
  local A, B, C, D = 0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476
  for chunk = 1, #buf, 64 do
    local X = {}
    for i = 0, 15 do
      local j = chunk + i * 4
      X[i + 1] = bytes2word(buf[j], buf[j+1], buf[j+2], buf[j+3])
    end
    local a, b, c, d = A, B, C, D
    for i = 0, 63 do
      local f, g
      if i <= 15 then f = F(b,c,d); g = i
      elseif i <= 31 then f = G(b,c,d); g = (5 * i + 1) % 16
      elseif i <= 47 then f = H(b,c,d); g = (3 * i + 5) % 16
      else f = I(b,c,d); g = (7 * i) % 16
      end
      local t = d
      d = c
      c = b
      b = band(b + lrotate(band(a + f + K[i + 1] + X[g + 1], 0xFFFFFFFF), S[i + 1]), 0xFFFFFFFF)
      a = t
    end
    A, B, C, D = band(A + a, 0xFFFFFFFF), band(B + b, 0xFFFFFFFF), band(C + c, 0xFFFFFFFF), band(D + d, 0xFFFFFFFF)
  end
  local function word2bytes(w)
    return band(w, 0xFF), band(rshift(w, 8), 0xFF), band(rshift(w, 16), 0xFF), band(rshift(w, 24), 0xFF)
  end
  return string.char(word2bytes(A), word2bytes(B), word2bytes(C), word2bytes(D))
end

local gcrypt
do
  local ok, mod = pcall(require, "luagcrypt")
  gcrypt = ok and mod or nil
end

local function derive_aes_key(pinfo)
  local nonce_key = tostring(pinfo.src) .. ":" .. pinfo.src_port
  local nonce = (_G.nonce[nonce_key] or "")
  local raw_key = nonce .. "-" .. bc_protocol.prefs.key
  local key_hex
  if gcrypt then
    local hasher = gcrypt.Hash(gcrypt.MD_MD5)
    hasher:write(raw_key)
    key_hex = hexencode(hasher:read())
  else
    key_hex = hexencode(md5_binary(raw_key))
  end
  return string.sub(key_hex, 1, 16)
end

local function aes_decrypt(data, pinfo)
  local key = derive_aes_key(pinfo)
  local iv = "0123456789abcdef"
  local ciphertext = data

  -- Prefer Wireshark's built-in GcryptCipher (no luagcrypt needed)
  if GcryptCipher and GCRY_CIPHER_AES256 and GCRY_CIPHER_MODE_CFB then
    local ok, decrypted = pcall(function()
      local cipher = GcryptCipher.open(GCRY_CIPHER_AES256, GCRY_CIPHER_MODE_CFB, 0)
      cipher:setkey(ByteArray.new(key))
      cipher:setiv(ByteArray.new(iv))
      return cipher:decrypt(nil, ByteArray.new(ciphertext, true))
    end)
    if ok and decrypted and decrypted:len() > 0 then
      return decrypted
    end
  end

  -- Fallback: luagcrypt (optional, build from source)
  if gcrypt then
    local cipher = gcrypt.Cipher(gcrypt.CIPHER_AES256, gcrypt.CIPHER_MODE_CFB)
    cipher:setkey(key)
    cipher:setiv(iv)
    local decrypted = cipher:decrypt(ciphertext)
    return ByteArray.new(decrypted, true)
  end

  return ByteArray.new("", true)
end

local function xml_decrypt(ba, offset)
  local key = "\031\045\060\075\090\105\120\255" -- 1f, 2d, 3c, 4b, 5a, 69, 78 ,ff
  local e = ByteArray.new()
  e:set_size(ba:len())
  for i=0,ba:len() - 1 do
    e:set_index(i, bit32.bxor(bit32.band(offset, 0xFF), bit32.bxor(ba:get_index(i), key:byte(((i + offset) % 8) + 1))))
  end
  return e
end

local function get_header_len(buffer)
  local magic = buffer(0, 4):le_uint()
  if magic ~= 0x0abcdef0 and magic ~= 0x0fedcba0 then
    -- Unknown magic
    return -1 -- No header found
  end
  local header_len = header_lengths[buffer(18, 2):le_uint()]
  if (not header_len) then
    return 20
  end
  return header_len
end

local function get_header(buffer)
  -- bin_offset is either nil (no binary data) or nonzero
  local bin_offset = nil
  local return_code = nil
  local encr_xml = nil
  local header_len = header_lengths[buffer(18, 2):le_uint()]
  if header_len == 24 then
    bin_offset = buffer(20, 4):le_uint() -- if NHD-805/806 legacy protocol 30 30 30 30 aka "0000"
    return_code =  buffer(16, 2):le_uint()
  else
    encr_xml = buffer(16, 1):le_uint()
  end
  local msg_type = buffer(4, 4):le_uint()
  local stream_text = "HD (Clear)"
  if buffer(13, 1):le_uint() == 1 then
    stream_text = "SD (Fluent)"
  end
  return {
    magic = buffer(0, 4):le_uint(),
    msg_type = buffer(4, 4):le_uint(),
    msg_type_str = message_types[msg_type] or "unknown",
    message_understood = message_types[msg_type] ~= nil and 1 or 0,
    msg_len = buffer(8, 4):le_uint(),
    encrypt_xml = encr_xml,
    channel_id = buffer(12, 1):le_uint(),
    enc_offset = buffer(12, 1):le_uint(),
    stream_type = stream_text,
    msg_handle = buffer(14, 2):le_uint(),
    msg_cls = buffer(18, 2):le_uint(),
    status_code = return_code,
    class = message_classes[buffer(18, 2):le_uint()] or "unknown",
    header_len = get_header_len(buffer(0, nil)),
    bin_offset = bin_offset,
  }
end

local function process_header(buffer, headers_tree)
  local header_data = get_header(buffer)
  local header = headers_tree:add(bc_protocol, buffer(0, header_data.header_len),
    "Baichuan Message Header, length: " .. header_data.header_len .. ", type " .. header_data.msg_type)
  local stream_text = " HD (Clear)"
  if buffer(13, 1):le_uint() == 1 then
    stream_text = " SD (Fluent)"
  end
  header:add_le(magic_bytes, buffer(0, 4))
  header:add_le(message_id,  buffer(4, 4))
        :append_text(" (" .. header_data.msg_type_str .. ")")
  header:add_le(message_len, buffer(8, 4))

  header:add_le(xml_enc_offset, buffer(12, 1))
        :append_text(" (& 0xF == " .. bit32.band(header_data.enc_offset, 0xF) .. ")")

  header:add_le(channel_id, buffer(12, 1))
  header:add_le(stream_id, buffer(13, 1))
        :append_text(stream_text)
  header:add_le(msg_handle, buffer(14, 2))

  header:add_le(message_class, buffer(18, 2)):append_text(" (" .. header_data.class .. ")")

  if header_data.header_len == 24 then
    header:add_le(status_code, buffer(16, 2))
    header:add_le(f_bin_offset, buffer(20, 4))
  else
    header:add_le(encrypt_xml, buffer(16, 1))
  end

  header:add(message_understood, header_data.message_understood):set_generated()
  return header_data.header_len
end

local function process_body(header, body_buffer, bc_subtree, pinfo)
  if header.msg_len == 0 then
    return
  end

  local body = bc_subtree:add(bc_protocol, body_buffer(0,header.msg_len),
    "Baichuan Message Body, " .. header.class .. ", length: " .. header.msg_len .. ", type " .. header.msg_type)

  -- Device report: sub-command at body offset 4 (handleDeviceReportCmds iVar1)
  if device_report_msg_ids[header.msg_type] and header.msg_len >= 8 then
    local subcmd = body_buffer(4, 4):le_uint()
    local label = report_subcmd_names[subcmd] or ("Report sub-command 0x" .. string.format("%x", subcmd))
    body:add_le(report_subcmd, body_buffer(4, 4)):append_text(" (" .. label .. ")")
  end

  if header.class == "legacy" then
    if header.msg_type == 1 then
      body:add_le(username, body_buffer(0, 32))
      body:add_le(password, body_buffer(0 + 32, 32))
    end
  else
    local xml_len = header.bin_offset
    if xml_len == nil then
      xml_len = header.msg_len
    end
    local xml_buffer = body_buffer(0, xml_len)
    if xml_len > 0 then
      local body_tvb = xml_buffer:tvb("Meta Payload")
      body:add(body_tvb(), "Meta Payload")
      if xml_len >= 4 then
        if aes_decrypt(xml_buffer:raw(0,5), pinfo):raw() == "<?xml" then -- AES encrypted
					local ba = xml_buffer:bytes()
          local decrypted = aes_decrypt(ba:raw(), pinfo)
          body_tvb = decrypted:tvb("Decrypted XML (in Meta Payload)")
          -- Create a tree item that, when clicked, automatically shows the tab we just created
          body:add(body_tvb(), "Decrypted XML (in Meta Payload)")
          Dissector.get("xml"):call(body_tvb, pinfo, body)
        elseif xml_decrypt(xml_buffer(0,5):bytes(), header.enc_offset):raw() == "<?xml" then -- Encrypted xml found
          local ba = xml_buffer:bytes()
          local decrypted = xml_decrypt(ba, header.enc_offset)
          local new_noonce = string.match(decrypted:raw(), "[<]nonce[>][ \t\n]*([^ \t\n<]+)[ \t\n]*[<][/]nonce[>]")
          local nonce_key = tostring(pinfo.src) .. ":" .. pinfo.src_port
          _G.nonce[nonce_key] = new_noonce;
          nonce_key = tostring(pinfo.dst) .. ":" .. pinfo.dst_port
          _G.nonce[nonce_key] = new_noonce;
          body_tvb = decrypted:tvb("Decrypted XML (in Meta Payload)")
          -- Create a tree item that, when clicked, automatically shows the tab we just created
          body:add(body_tvb(), "Decrypted XML (in Meta Payload)")
          Dissector.get("xml"):call(body_tvb, pinfo, body)
        elseif xml_buffer(0,5):string() == "<?xml" then  -- Unencrypted xml
          body:add(body_tvb(), "XML (in Meta Payload)")
          Dissector.get("xml"):call(body_tvb, pinfo, body)
        else
          body:add(body_tvb(), "Binary (in Meta Payload)")
        end
      end
    end

    if header.bin_offset ~= nil then
      local bin_len = header.msg_len - header.bin_offset
      if bin_len > 0 then
        local binary_buffer = body_buffer(header.bin_offset, bin_len) -- Don't extend beyond msg size
        local body_tvb = binary_buffer:tvb("Main Payload");
        body:add(body_tvb(), "Main Payload")
        if bin_len > 4 then
          if aes_decrypt(binary_buffer:raw(0,5), pinfo):raw() == "<?xml" then -- AES encrypted
            local ba = binary_buffer:bytes()
            local decrypted = aes_decrypt(ba:raw(), pinfo)
            body_tvb = decrypted:tvb("Decrypted XML (in Binary Payload)")
            -- Create a tree item that, when clicked, automatically shows the tab we just created
            body:add(body_tvb(), "Decrypted XML (in Binary Payload)")
            Dissector.get("xml"):call(body_tvb, pinfo, body)
          elseif xml_decrypt(binary_buffer(0,5):bytes(), header.enc_offset):raw() == "<?xml" then -- Encrypted xml found
            local decrypted = xml_decrypt(binary_buffer:bytes(), header.enc_offset)
            body_tvb = decrypted:tvb("Decrypted XML (in Main Payload)")
            -- Create a tree item that, when clicked, automatically shows the tab we just created
            body:add(body_tvb(), "Decrypted XML (in Main Payload)")
            Dissector.get("xml"):call(body_tvb, pinfo, body)
          elseif binary_buffer(0,5):string() == "<?xml" then  -- Unencrypted xml
            body:add(body_tvb(), "XML (in Main Payload)")
            Dissector.get("xml"):call(body_tvb, pinfo, body)
          else
            body:add(body_tvb(), "Binary (in Main Payload)")
          end
        end
      end
    end
  end
end


-- UDP CONTENT
local udp_fragments = {}

local function rshift(x, by)
  return math.floor(x / 2 ^ by)
end

local function udp_decrypt(data, tid)
  local result = ByteArray.new()
  result:set_size(data:len())
  local key = {
    0x1f2d3c4b, 0x5a6c7f8d,
    0x38172e4b, 0x8271635a,
    0x863f1a2b, 0xa5c6f7d8,
    0x8371e1b4, 0x17f2d3a5
  }

  for i=1, 8 do
    key[i] = key[i] + tid
  end

  local i = data:len() + 3
  if i < 0 then
    i = data:len() + 6
  end

  for x=0, rshift(i, 2) do
    local index = bit32.band(x, 7)
    local xor_key_word = key[index + 1]
    for b=0, 3 do
      local byte_index = x * 4 + b
      local val = data:get_index(byte_index)
      local key_byte = bit32.extract(xor_key_word, b*8, 8)
      val = bit32.bxor(key_byte, val)
      result:set_index(byte_index, val)
      if byte_index >= data:len() - 1 then
        return result
      end
    end
  end
  return result
end


local function get_udp_header_len(buffer)
  local udpmagic = buffer(1, 3):le_uint()
  if udpmagic ~= 0x2a87cf then
    return 0
  else
    local udptype = buffer(0, 1):le_uint()
    if udptype == 0x3a then
      return 20
    elseif udptype == 0x31 then
      return 20
    elseif udptype == 0x20 then
      return 28
    elseif udptype == 0x10 then
      return 20
    else
      return -1
    end
  end
end

local function get_udp_header(buffer)
  local udp_class = buffer(0, 1):le_uint()
  local l_udp_magic = buffer(1, 3):le_uint()
  local length = get_udp_header_len(buffer)
  local l_udp_size = nil
  local udp_unknown1 = nil
  local l_udp_tid = nil
  local l_udp_checksum = nil
  local udp_unknown2 = nil
  local udp_unknown3 = nil
  local udp_unknown4 = nil
  local l_udp_last_ack_packet = nil
  local l_udp_ack_payload_size = nil
  local l_udp_connection_id = nil
  local l_udp_packet_count = nil
  if udp_class == 0x3a then
    l_udp_size = buffer(4, 4):le_uint()
    udp_unknown1 = buffer(8, 4):le_uint()
    l_udp_tid = buffer(12, 4):le_uint()
    l_udp_checksum = buffer(16, 4):le_uint()
  elseif udp_class == 0x31 then
    l_udp_size = buffer(4, 4):le_uint()
    udp_unknown1 = buffer(8, 4):le_uint()
    l_udp_tid = buffer(12, 4):le_uint()
    l_udp_checksum = buffer(16, 4):le_uint()
  elseif udp_class == 0x20 then
    l_udp_connection_id = buffer(4, 4):le_uint()
    udp_unknown1 = buffer(8, 4):le_uint()
    udp_unknown2 = buffer(12, 4):le_uint()
    l_udp_last_ack_packet = buffer(16, 4):le_uint()
    udp_unknown3 = buffer(20, 4):le_uint()
    l_udp_ack_payload_size = buffer(24, 4):le_uint()
  elseif udp_class == 0x10 then
    l_udp_connection_id = buffer(4, 4):le_uint()
    udp_unknown1 = buffer(8, 4):le_uint()
    l_udp_packet_count = buffer(12, 4):le_uint()
    l_udp_size = buffer(16, 4):le_uint()
  end
  return {
    length = length,
    class = udp_class,
    magic = l_udp_magic,
    payload_size = l_udp_size,
    unknown1 = udp_unknown1,
    unknown2 = udp_unknown2,
    unknown3 = udp_unknown3,
    unknown4 = udp_unknown4,
    tid = l_udp_tid,
    checksum = l_udp_checksum,
    connection_id = l_udp_connection_id,
    packet_count = l_udp_packet_count,
    last_ack_packet = l_udp_last_ack_packet,
    ack_payload_size = l_udp_ack_payload_size
  }
end

local function process_udp_header(buffer, headers_tree)
  local header_data = get_udp_header(buffer)
  local header = headers_tree:add(bc_protocol, buffer(0, header_data.length),
    "Baichuan UDP Header, length: " .. header_data.length .. ", type " .. header_data.class)
  header:add_le(udp_magic, buffer(0,4))
  header:add_le(udp_type, buffer(0,1))
  if header_data.class == 0x3a then
    header:add_le(udp_size, buffer(4, 4))
    header:add_le(udp_unknown,buffer(8, 4))
    header:add_le(udp_tid, buffer(12, 4))
    header:add_le(udp_checksum, buffer(16, 4))
  elseif header_data.class == 0x31 then
    header:add_le(udp_size, buffer(4, 4))
    header:add_le(udp_unknown,buffer(8, 4))
    header:add_le(udp_tid, buffer(12, 4))
    header:add_le(udp_checksum, buffer(16, 4))
  elseif header_data.class == 0x20 then
    header:add_le(udp_connection_id, buffer(4, 4))
    header:add_le(udp_unknown, buffer(8, 4))
    header:add_le(udp_unknown, buffer(12, 4))
    header:add_le(udp_last_ack_packet, buffer(16, 4))
    header:add_le(udp_unknown, buffer(20, 4))
    header:add_le(udp_ack_payload_size, buffer(24, 4))
  elseif header_data.class == 0x10 then
    header:add_le(udp_connection_id, buffer(4, 4))
    header:add_le(udp_unknown, buffer(8, 4))
    header:add_le(udp_packet_count, buffer(12, 4))
    header:add_le(udp_size, buffer(16, 4))
  end
  return header_data.length
end

local function process_bc_message(buffer, pinfo, tree)
  pinfo.cols.protocol = bc_protocol.name

  local sub_buffer = buffer
  local table_msg_type_str = {}
  local table_msg_type = {}

  local continue_loop = true
  while ( continue_loop )
  do
    local header_len = get_header_len(sub_buffer(0, nil))

    if header_len >= 0 then
      -- Valid magic and header found
      local header = get_header(sub_buffer(0, nil))
      table.insert(table_msg_type_str, header.msg_type_str)
      table.insert(table_msg_type, header.msg_type)

      -- Get full header and body
      local full_body_len =  header.msg_len + header.header_len

      local remaining = sub_buffer:len() - header.header_len

      local full_body_len = header.header_len + header.msg_len
      local bc_subtree = tree:add(bc_protocol, sub_buffer(0, full_body_len),
        "Baichuan IP Camera Protocol, " .. header.msg_type_str .. ":" .. header.msg_type .. " message")
      process_header(sub_buffer, bc_subtree)
      if header.header_len < sub_buffer:len() then
        local body_buffer = sub_buffer(header.header_len,nil)
        process_body(header, body_buffer, bc_subtree, pinfo)

        remaining = body_buffer:len() - header.msg_len
      end

      -- Remaining bytes?
      if remaining == 0 then
        continue_loop = false
      else
        sub_buffer = sub_buffer(full_body_len, nil)
      end
    else
      return
    end
  end

  local msg_type_strs = table.concat(table_msg_type_str, ",")
  local msg_types = table.concat(table_msg_type, ",")
  pinfo.cols['info'] = msg_type_strs .. ", type " .. msg_types
  return
end

local function is_complete_bc_message(buffer)
  local length = buffer:len()
  if length == 0 then
    return "DONE"
  end

  local sub_buffer = buffer

  local continue_loop = true
  while ( continue_loop )
  do

    -- Get min bytes for a magic and header len
    if sub_buffer:len() < 20 then
      -- Need more bytes but we don't have a header to learn how many bytes
      return "+1"
    end
    local header_len = get_header_len(sub_buffer(0, nil))

    if header_len >= 0 then
      -- Valid magic and header found

      -- Ensure min bytes for full header
      if sub_buffer:len() < header_len then
        -- Need more bytes
        return header_len - sub_buffer:len()
      end


      local header = get_header(sub_buffer(0, nil))

      -- Get full header and body
      local full_body_len =  header.msg_len + header.header_len
      if sub_buffer:len() < full_body_len then
        return full_body_len - sub_buffer:len()
      end

      local remaining = sub_buffer:len() - header.header_len
      if header.header_len < sub_buffer:len() then
        local body_buffer = sub_buffer(header.header_len, nil)
        remaining = body_buffer:len() - header.msg_len
      end

      -- Remaning bytes?
      if remaining == 0 then
        continue_loop = false
      else
        sub_buffer = sub_buffer(full_body_len, sub_buffer:len() - full_body_len)
      end
    else
      return "NOMAGIC"
    end
  end
  return "DONE"
end

local function udp_reassemple(udp_header, subbuffer, more, pinfo, tree)
  -- Cache udp message for later lookup
  local con_id = udp_header.connection_id
  if udp_fragments[con_id] == nil then
    udp_fragments[con_id] = {}
  end
  local mess_id = udp_header.packet_count
  if udp_fragments[con_id][mess_id] == nil then
    udp_fragments[con_id][mess_id] = {}
  end
  udp_fragments[con_id][mess_id]['result'] = more
  udp_fragments[con_id][mess_id]['message_id'] = mess_id
  udp_fragments[con_id][mess_id]['buffer'] = subbuffer:bytes()

  -- Go backwards from current ID until:
  -- I hit a result that is not NOMAGIC
  -- Can be myself
  local start_idx = mess_id
  local start_fragment = udp_fragments[con_id][start_idx]
  while start_fragment.result == "NOMAGIC" do
    start_idx = start_idx -1
    start_fragment = udp_fragments[con_id][start_idx]
    if start_fragment == nil then
      break
    end
  end

  if start_fragment ~= nil then -- Found a starting fragment
    local needed = start_fragment.result
    if needed == "DONE" then
      if start_fragment.message_id == udp_header.packet_count then
        local rtvb = start_fragment.buffer:tvb("UDP Reassembly")
        process_bc_message(rtvb, pinfo, tree)
        parse_and_collect_replay(rtvb, pinfo)
      end
    elseif needed == "+1" then
      -- Cannot handle in UDP...
      -- Only happens in off chance not enough data for
      -- even the header
      -- Never observed to date
      return
    else
      -- pinfo.cols['info'] = "SEARCHING"
      local next_id = start_fragment.message_id + 1
      local reassembled = ByteArray.new()
      local total_packet = 1
      reassembled = reassembled .. start_fragment.buffer
      local target_len = reassembled:len() + start_fragment.result
      while reassembled:len() < target_len do
        local next_fragment = udp_fragments[con_id][next_id]
        if next_fragment ~= nil then
          reassembled = reassembled .. next_fragment.buffer
          total_packet = total_packet + 1
        else
          break
        end
      end
      if reassembled:len() >= target_len then
        start_fragment.result = "DONE"
        local rtvb = reassembled:tvb("Reassembled UDP")
        process_bc_message(rtvb, pinfo, tree)
        parse_and_collect_replay(rtvb, pinfo)
      end
    end
  end
end

function bc_protocol.init ()
   udp_fragments = {}
   _replay_packets = {}
end

function bc_protocol.dissector(buffer, pinfo, tree)
  local subbuffer = nil
  local udp_header = nil
  if pinfo.can_desegment == 0 then -- UDP
    local udp_header_len = get_udp_header_len(buffer)
    if udp_header_len > 0 then
      udp_header = get_udp_header(buffer(0, udp_header_len))
      process_udp_header(buffer(0, udp_header_len), tree)
      if udp_header.class == 0x3a then
        local decrypted_bytes = udp_decrypt(buffer(udp_header_len, nil):bytes(), udp_header.tid)
        local decryped_tvb = decrypted_bytes:tvb("UDP Decrypted Message")
        local subtree = tree:add(bc_protocol, decryped_tvb, "UDP Message Data")
        Dissector.get("xml"):call(decryped_tvb, pinfo, subtree)
        pinfo.cols.protocol = bc_protocol.name .. " UDP Heartbeat"
      elseif udp_header.class == 0x31 then
        pinfo.cols.protocol = bc_protocol.name .. " UDP Relay"

      elseif udp_header.class == 0x20 then
        pinfo.cols.protocol = bc_protocol.name .. " UDP ACK"
        if udp_header.ack_payload_size > 0 then
          tree:add(bc_protocol, buffer(28,udp_header.ack_payload_size), "BcUdp Ack Payload")
        end
      else
        subbuffer = buffer(udp_header_len, nil)
      end
    else
      subbuffer = buffer(udp_header_len, nil)
    end
  else
    subbuffer = buffer
  end
  if subbuffer ~= nil then
    local more = is_complete_bc_message(subbuffer)
    if pinfo.can_desegment == 1 then -- TCP can use the desegment method
      if more == "DONE" then
        process_bc_message(subbuffer, pinfo, tree)
        parse_and_collect_replay(subbuffer, pinfo)
        return
      elseif more == "+1" then
        pinfo.desegment_len = DESEGMENT_ONE_MORE_SEGMENT
        pinfo.desegment_offset = 0
        return subbuffer:len()
      elseif more == "NOMAGIC" then
        return
      else
        pinfo.desegment_len = more
        pinfo.desegment_offset = 0
        return subbuffer:len()
      end
    else -- UDP can not use the desegment method, must reassemble manually
      if udp_header ~= nil then
        if udp_header.class == 0x10 then -- Continuable udp class
          udp_reassemple(udp_header, subbuffer, more, pinfo, tree)
        end
      end
    end
  end
end
--- END UDP CONTENT

local added_udp_ports = {}
local function heuristic_checker_udp(buffer, pinfo, tree)
    -- guard for length
    local length = buffer:len()
    if length < 4 then return false end
    local potential_magic = buffer(0,4):le_uint()

    if potential_magic ~= 0x2a87cf3a  and
        potential_magic ~= 0x2a87cf20 and
        potential_magic ~= 0x2a87cf10 and
        potential_magic ~= 0x2a87cf31 then

      return false
    end

    if added_udp_ports[pinfo.dst_port] == nil then
      table.insert(added_udp_ports, pinfo.dst_port)
      DissectorTable.get("udp.port"):add(pinfo.dst_port, bc_protocol)
    end
    if added_udp_ports[pinfo.src_port] == nil then
      table.insert(added_udp_ports, pinfo.src_port)
      DissectorTable.get("udp.port"):add(pinfo.src_port, bc_protocol)
    end

    bc_protocol.dissector(buffer, pinfo, tree)
    return true
end

local added_tcp_ports = {}

local function heuristic_checker_tcp(buffer, pinfo, tree)
    -- guard for length
    local length = buffer:len()
    if length < 4 then return false end
    local potential_magic = buffer(0,4):le_uint()
    if potential_magic ~= 0x0abcdef0 and
        potential_magic ~= 0x0fedcba0  then
      return false
    end

    if added_tcp_ports[pinfo.dst_port] == nil then
      table.insert(added_tcp_ports, pinfo.dst_port)
      DissectorTable.get("tcp.port"):add(pinfo.dst_port, bc_protocol)
    end
    if added_tcp_ports[pinfo.src_port] == nil then
      table.insert(added_tcp_ports, pinfo.src_port)
      DissectorTable.get("tcp.port"):add(pinfo.src_port, bc_protocol)
    end

    bc_protocol.dissector(buffer, pinfo, tree)
    return true
end

-----
-- Replay stream export: collect replay binary (msg 5/8/381) and export to file.
-----
local REPLAY_MSG_IDS = { [5]=true, [8]=true, [381]=true }
local REPLAY_END_STATUS = { [300]=true, [331]=true }
local _replay_packets = {}  -- list of {pinfo, status, payload} in packet order

function parse_and_collect_replay(tvb, pinfo)
  if tvb:len() < 20 then return end
  local offset = 0
  while offset + 20 <= tvb:len() do
    local sub = tvb(offset, nil)
    local header_len = get_header_len(sub(0, nil))
    if header_len < 0 or offset + header_len > tvb:len() then break end
    local header = get_header(sub(0, nil))
    local full_len = header.header_len + header.msg_len
    if offset + full_len > tvb:len() then break end
    if REPLAY_MSG_IDS[header.msg_type] and header.status_code and (header.status_code == 200 or REPLAY_END_STATUS[header.status_code]) then
      if header.bin_offset and header.msg_len > header.bin_offset then
        local bin_len = header.msg_len - header.bin_offset
        local payload = sub(header.header_len + header.bin_offset, bin_len):bytes():raw()
        table.insert(_replay_packets, { num = pinfo.number, status = header.status_code, payload = payload })
      end
    end
    offset = offset + full_len
  end
end

bc_protocol.prefs.replay_export_dir = Pref.string("Replay export directory", "",
  "Directory path to save exported replay streams (replay_1.bin, replay_2.bin). Empty = current directory.")

local function reassemble_replay_stream(packets)
  table.sort(packets, function(a,b) return a.num < b.num end)
  local out = {}
  local skip_first_32 = true
  for _, p in ipairs(packets) do
    if skip_first_32 and #p.payload == 32 then
      skip_first_32 = false
    else
      skip_first_32 = false
      out[#out + 1] = p.payload
    end
    if REPLAY_END_STATUS[p.status] then break end
  end
  return table.concat(out, "")
end

local function export_replay_streams()
  if #_replay_packets == 0 then
    if gui and gui.message_box then
      gui.message_box("No replay packets found in this capture.", "Export", "ok")
    end
    return
  end
  local dir = bc_protocol.prefs.replay_export_dir
  if dir and dir ~= "" and not (dir:match("/$")) then dir = dir .. "/" end
  dir = dir or ""
  -- Split into streams: each stream ends at status 300/331 or at end of list
  local streams = {}
  local current = {}
  for _, p in ipairs(_replay_packets) do
    current[#current + 1] = p
    if REPLAY_END_STATUS[p.status] then
      streams[#streams + 1] = current
      current = {}
    end
  end
  if #current > 0 then streams[#streams + 1] = current end
  local written = 0
  for i, stream_packets in ipairs(streams) do
    local raw = reassemble_replay_stream(stream_packets)
    if #raw > 0 then
      local ext = "bin"
      if raw:sub(5,8) == "ftyp" then ext = "mp4" end
      local fname = dir .. "replay_" .. i .. "." .. ext
      local f = io.open(fname, "wb")
      if f then
        f:write(raw)
        f:close()
        written = written + 1
      end
    end
  end
  if gui and gui.message_box and written > 0 then
    gui.message_box("Exported " .. written .. " replay stream(s) to " .. (dir ~= "" and dir or "(current directory)"), "Export", "ok")
  end
end

register_menu("Export Baichuan Replay to File", export_replay_streams, MENU_TOOLS_UNSORTED)

bc_protocol:register_heuristic("udp", heuristic_checker_udp)
bc_protocol:register_heuristic("tcp", heuristic_checker_tcp)
-- DissectorTable.get("tcp.port"):add(53959, bc_protocol) -- change to your own custom port

-- DissectorTable.get("udp.port"):add(2000, bc_protocol)
-- DissectorTable.get("udp.port"):add(2015, bc_protocol)
-- DissectorTable.get("udp.port"):add(2018, bc_protocol)
-- DissectorTable.get("udp.port"):add(2000, bc_protocol)
-- DissectorTable.get("udp.port"):add(9999, bc_protocol)
