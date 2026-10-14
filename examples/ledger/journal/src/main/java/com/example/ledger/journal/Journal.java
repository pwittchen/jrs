package com.example.ledger.journal;

import com.example.ledger.core.Ledger;
import com.example.ledger.core.Money;
import com.example.ledger.core.Posting;
import com.example.ledger.core.Transaction;
import java.io.IOException;
import java.io.Reader;
import java.time.LocalDate;
import java.time.format.DateTimeParseException;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import org.apache.commons.csv.CSVFormat;
import org.apache.commons.csv.CSVRecord;

/**
 * Reads a journal kept as CSV, one posting per row, the rows of one
 * transaction sharing its {@code id}:
 *
 * <pre>
 * id,date,description,account,amount,currency
 * 1,2026-01-01,Salary,assets:bank,2500.00,EUR
 * 1,2026-01-01,Salary,income:salary,-2500.00,EUR
 * </pre>
 */
public final class Journal {

    private static final CSVFormat FORMAT = CSVFormat.DEFAULT.builder()
            .setHeader()
            .setSkipHeaderRecord(true)
            .setTrim(true)
            .setIgnoreEmptyLines(true)
            .get();

    private Journal() {}

    /** The ledger the journal records, its transactions in the order they first appear. */
    public static Ledger read(Reader reader) throws IOException {
        Map<String, List<CSVRecord>> byId = new LinkedHashMap<>();
        for (CSVRecord row : FORMAT.parse(reader)) {
            byId.computeIfAbsent(row.get("id"), id -> new ArrayList<>()).add(row);
        }
        Ledger ledger = new Ledger();
        for (Map.Entry<String, List<CSVRecord>> entry : byId.entrySet()) {
            ledger.record(transaction(entry.getKey(), entry.getValue()));
        }
        return ledger;
    }

    private static Transaction transaction(String id, List<CSVRecord> rows) {
        CSVRecord first = rows.get(0);
        List<Posting> postings = new ArrayList<>();
        for (CSVRecord row : rows) {
            postings.add(new Posting(row.get("account"), Money.parse(row.get("amount"), row.get("currency"))));
        }
        try {
            return new Transaction(LocalDate.parse(first.get("date")), first.get("description"), postings);
        } catch (DateTimeParseException | IllegalArgumentException e) {
            throw new JournalException("transaction " + id + " (line " + first.getRecordNumber() + "): "
                    + e.getMessage(), e);
        }
    }

    /** A journal that does not describe a ledger. */
    public static final class JournalException extends IllegalArgumentException {
        private static final long serialVersionUID = 1L;

        JournalException(String message, Throwable cause) {
            super(message, cause);
        }
    }
}
