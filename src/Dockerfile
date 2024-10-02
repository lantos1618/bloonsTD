# Step 1: Build the Rust application
FROM rust:1.72 AS builder

# Set the working directory inside the container
WORKDIR /app

# Copy the Cargo.toml and Cargo.lock files
COPY Cargo.toml Cargo.lock ./

# Create a dummy src/main.rs file to cache dependencies
RUN mkdir src && echo "fn main() {}" > src/main.rs

# Build dependencies to cache them
RUN cargo build --release || true

# Now copy the actual source code
COPY . .

# Build the final release of the application
RUN cargo build --release

# Step 2: Create a lightweight image for the runtime
FROM debian:buster-slim

# Install necessary runtime dependencies (if needed)
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*

# Set the working directory inside the final container
WORKDIR /app

# Copy the compiled Rust binary from the builder stage
COPY --from=builder /app/target/release/myapp .

# Copy static files (for your static HTML content)
COPY static /app/static

# Expose the port the web app runs on
EXPOSE 5000

# Set the default command to run the application
CMD ["./myapp"]
