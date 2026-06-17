import os

def create_llm_context(output_file="llm_context.txt"):
    # Файлы, которые мы хотим включить дополнительно
    extra_files = ["README.md", "Cargo.toml"]
    src_dir = "src"
    
    with open(output_file, "w", encoding="utf-8") as outfile:
        # 1. Добавляем дополнительные файлы
        for filename in extra_files:
            if os.path.exists(filename):
                outfile.write(f"--- FILE: {filename} ---\n")
                with open(filename, "r", encoding="utf-8") as f:
                    outfile.write(f.read())
                outfile.write("\n\n")

        # 2. Добавляем содержимое папки src
        for root, dirs, files in os.walk(src_dir):
            for file in files:
                file_path = os.path.join(root, file)
                
                # Игнорируем скрытые файлы (например .DS_Store)
                if file.startswith('.'):
                    continue
                
                outfile.write(f"--- FILE: {file_path} ---\n")
                try:
                    with open(file_path, "r", encoding="utf-8") as f:
                        outfile.write(f.read())
                except Exception as e:
                    outfile.write(f"Could not read file: {e}")
                outfile.write("\n\n")
    
    print(f"Готово! Весь код собран в файл: {output_file}")

if __name__ == "__main__":
    create_llm_context()