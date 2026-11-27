# Testing Payout Admin in Regtest

## Dependencies

Use macOS or Debian.

You need the **tester kit**. You can build it as described in [build.md](build.md), or you unzip a kit that you received.

You need a **Ledger** with Bitcoin Test version 2.4.6. You can install the app via Ledger Wallet.

On Debian, you need to install the following programs:

```sh
sudo apt install libwebkit2gtk-4.1-0 libgtk-3-0 ledger-wallets-udev xclip wl-clipboard lsof
```

You use one terminal for the whole test. Run every command in the kit folder.
If you built the kit from source, the location is `docs/payout-admin/target/payout-admin-test-kit/`.

## Setup

### Step 1 — Start Bitcoin Core and the App

1. Open a terminal in the kit folder.
2. Run the start script. This starts Bitcoin Core, Electrum, the kit’s ASM endpoint and the app.

   ```sh
   ./start.sh
   ```

3. The app opens. Close the settings menu if it opened.

### Step 2 — Create the Admin Keys

Your Ledger is admin 1. The remaining admins 2 and 3 are hot wallets.

1. If you want to wipe the admin keys from a previous session, then run the following command. Skip this step on your first run.

   ```sh
   rm -rf /tmp/payout-signers
   ```

2. Run the following commands to create the keys of admins 2 and 3.

   ```sh
   ./admin2
   ./admin3
   ```

### Step 3 — Read the Ledger Identity

1. Connect your Ledger via USB. Unlock it and open the Bitcoin Test app.
2. In the app, click **Ledger** and then **Connect wallet**.
3. Scroll down and select **Payout Administrator**.
4. Click **Read Ledger identity**. On the Ledger, approve exporting the account public key if it asks.

**You should see:** the app shows the Ledger account xpub and fingerprint.

### Step 4 — Set Up the Bridge

1. Run the following command to prepare the bridge for testing.

   ```sh
   ./regtest-bridge setup
   ```

2. Copy the account xpub from the app into the first prompt.
3. Copy the fingerprint from the app into the second prompt.
4. Wait until setup says to start the coordinator. Leave the Ledger idle while you wait.

If setup fails, run `./stop.sh` and start again from Step 1.

### Step 5 — Start the Coordinator

1. Run the following command. The coordinator keeps running in the background and writes its log to `/tmp/payout-test/server.log`.

   ```sh
   env -u DATABASE_URL ./server > /tmp/payout-test/server.log 2>&1 &
   ```

### Step 6 — Sign In

1. In the app, click **Open settings**.
2. Select **Custom**.
3. Enter these values.
   - **Strata RPC URL:** `http://127.0.0.1:18502`
   - **Bitcoin RPC URL:** `http://127.0.0.1:18500`
   - **Electrum URL:** `tcp://127.0.0.1:18501`
   - **RPC User:** `user`
   - **RPC Password:** `password`
   - **Payout admin signers.toml path:** `/tmp/payout-test/signers.toml`
4. Click **Save**.
5. Click **Sign in on Ledger**.
6. On the Ledger, review the message and select **Sign message**.

**You should see:** the **Payout Admin** screen.

## Block Payouts Button

Each version 2 report identifies one ACK and the contest transaction ID for each claim to block. Claims may precede or follow the ACK. The app groups claims with the same operator and ACK evidence before splitting by size.

### Step 7 — Block Immature Claim

1. In the app, click **+ Block payouts**. The **Block payouts** screen opens.
2. Run the following command. It copies a report to the clipboard.

   ```sh
   ./regtest-bridge report 1 | ./pbcopy
   ```

3. In the app, paste the clipboard into the text field and click **Verify reports**. The report is rejected ("0 valid, 1 excluded").
4. Run the following command to mine blocks.

   ```sh
   ./regtest-bridge mine
   ```

5. In the app, click **Verify reports** again. If the report is still rejected, wait a few seconds and click it again. The report is accepted ("1 valid, 0 excluded").
6. Click **Sign on Ledger**.
7. On the Ledger, verify that each screen matches. Then click **reject**.
8. The app returns to the **Block payouts** screen. Click **Sign on Ledger** again.
9. On the Ledger, verify that each screen matches (claim inputs) and click **accept**.
10. On the Ledger, verify that each screen matches (fee inputs) and click **accept**.
11. The **Payout Admin** screen opens. Click the **Pending** tab.
12. Find the transaction you just made (1 / 2 signatures) and click **Details**.
13. Click **Copy PSBT (base64)**.
14. Run the following command. Admin 2 signs, and its signatures go to the clipboard.

    ```sh
    ./pbpaste | ./admin2 sign | ./pbcopy
    ```

15. In the app, click **Paste signatures**.
16. Paste the clipboard into the text field and click **Import**.

**You should see:** the transaction leaves the **Pending** tab and appears in the **Past** tab.

### Step 8 — Block 4 Claims, Exclude 1 Invalid Claim

1. In the app, click **+ Block payouts**. The **Block payouts** screen opens.
2. Run the following command.

   ```sh
   ./regtest-bridge report 2 | ./pbcopy
   ```

3. In the app, paste the clipboard into the text field and click **Verify reports**. 3 claims are accepted and 1 claim is rejected ("3 valid, 1 excluded").
4. Click **Sign on Ledger**.
5. On the Ledger, verify that each screen matches (claim inputs) and click **accept**.
6. On the Ledger, verify that each screen matches (fee inputs) and click **accept**.
7. The **Payout Admin** screen opens. Click the **Pending** tab.
8. Find the transaction you just made (1 / 2 signatures) and click **Details**.
9. Click **Copy PSBT (base64)**.
10. Run the following command. Admin 2 adds its signature to the PSBT.

    ```sh
    ./pbpaste | ./admin2 add-sig | ./pbcopy
    ```

11. In the app, click **Import PSBT**.
12. Paste the clipboard into the text field and click **Import**.

**You should see:** the transaction leaves the **Pending** tab and appears in the **Past** tab.

### Step 9 — Block 20 Claims

1. In the app, click **+ Block payouts**. The **Block payouts** screen opens.
2. Run the following command.

   ```sh
   ./regtest-bridge report 3 | ./pbcopy
   ```

3. In the app, paste the clipboard into the text field and click **Verify reports**. 20 claims are accepted ("20 valid, 0 excluded").
4. Click **Sign on Ledger**.
5. On the Ledger, verify that each screen matches (claim inputs) and click **accept**.
6. On the Ledger, verify that each screen matches (fee inputs) and click **accept**.
7. The **Payout Admin** screen opens. Click the **Pending** tab.
8. Find the transaction you just made (1 / 2 signatures) and click **Details**.
9. Click **Copy PSBT (base64)**.
10. Run the following command.

    ```sh
    ./pbpaste | ./admin2 add-sig | ./pbcopy
    ```

11. In the app, click **Import PSBT**.
12. Paste the clipboard into the text field and click **Import**.
13. Click the **Past** tab.

**You should see:** the three transactions from Steps 7 to 9 show **Unconfirmed**.

## Import PSBT Button

Each `psbt` command in the next steps creates fresh coins and mines blocks.

### Step 10 — Import PSBT Without Signatures

1. In the app, click **Import PSBT**.
2. Run the following command.

   ```sh
   ./regtest-bridge psbt | ./pbcopy
   ```

3. In the app, paste the clipboard into the text field and click **Import**.

**You should see:** "PSBT imported." The **Unverified** section of the **Pending** tab shows the transaction with 0 / 2 signatures.

### Step 11 — Import PSBT With Valid Signatures

1. In the app, click **Import PSBT**.
2. Run the following command.

   ```sh
   ./regtest-bridge psbt | ./admin2 add-sig | ./pbcopy
   ```

3. In the app, paste the clipboard into the text field and click **Import**.

**You should see:** "PSBT imported." The **Unverified** section of the **Pending** tab shows the transaction with 1 / 2 signatures.

### Step 12 — Import PSBT With Invalid Signatures

1. In the app, click **Import PSBT**.
2. Run the following command. Admin 3 adds an invalid signature.

   ```sh
   ./regtest-bridge psbt | ./admin3 add-bad-sig | ./pbcopy
   ```

3. In the app, paste the clipboard into the text field and click **Import**.

**You should see:** "PSBT imported." The **Unverified** section of the **Pending** tab shows the transaction with 0 / 2 signatures. The app drops the invalid signature.

## Past Transactions

### Step 13 — Confirm the Block Payouts Transactions

1. In the app, click the **Past** tab. The import steps may already have confirmed the three transactions.
2. Run the following command.

   ```sh
   ./regtest-bridge mine
   ```

**You should see:** within a minute, the three transactions show **Confirmed**.

## Stopping

### Step 14 — Stop Everything

1. Run the following command. It stops the app, the coordinator, Bitcoin Core, Electrum and the kit’s ASM endpoint. It deletes `/tmp/payout-test` and keeps the admin keys in `/tmp/payout-signers`.

   ```sh
   ./stop.sh
   ```
