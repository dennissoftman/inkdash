# Factory firmware backups

Files in this directory are full raw flash images read from the device before development flashing.

To restore a backup, use the ESP-IDF environment and write it at flash address `0x0`:

```sh
esptool --chip esp32s3 --port PORT write-flash 0x0 BACKUP.bin
```

Verify the intended device, port, backup filename, and checksum before restoring.
