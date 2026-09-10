package com.example.cart;

import java.util.ArrayList;
import java.util.Collections;
import java.util.List;

/** A shopping cart: its items, and the discounts on offer. Prices are in cents. */
public final class Cart {
    /** One line of the cart. */
    public record Item(String name, long unitCents, int quantity) {
        public Item {
            if (quantity <= 0) {
                throw new IllegalArgumentException("quantity must be positive: " + name);
            }
            if (unitCents < 0) {
                throw new IllegalArgumentException("price must not be negative: " + name);
            }
        }

        public long cents() {
            return unitCents * quantity;
        }
    }

    private final List<Item> items = new ArrayList<>();
    private final List<Discount> discounts = new ArrayList<>();

    public Cart add(String name, long unitCents, int quantity) {
        items.add(new Item(name, unitCents, quantity));
        return this;
    }

    public Cart offer(Discount discount) {
        discounts.add(discount);
        return this;
    }

    public List<Item> items() {
        return Collections.unmodifiableList(items);
    }

    public long subtotalCents() {
        return items.stream().mapToLong(Item::cents).sum();
    }

    /** The best discount on offer: they do not stack. */
    public long discountCents() {
        long subtotal = subtotalCents();
        return discounts.stream().mapToLong(d -> d.cents(this, subtotal)).max().orElse(0);
    }

    public long totalCents() {
        return subtotalCents() - discountCents();
    }
}
