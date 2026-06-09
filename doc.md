# 📖 Lume: Stylistic Synthesis & Retrieval-Augmented Generation (RAG) Architecture

This document describes the system design, philosophy, and step-by-step inference pipeline for style-faithful question answering using a combination of local indexing, a custom overfit style-synthesizer model, and a local LLM editor.

---

## 🎯 Design Philosophy: The Paradox of Overfitting

### Why Overfitting is Natively Bad in Machine Learning
In traditional machine learning, **overfitting** is the ultimate failure state. It occurs when a model learns the training data *too* well—memorizing the noise, specific examples, and exact phrasing of the dataset rather than extracting underlying generalizable concepts. 

The consequences of bad overfitting include:
* **Failure to Generalize**: The model fails completely when presented with new, unseen inputs because it expects them to match the exact patterns it memorized.
* **Brittleness**: The model lacks adaptability and repeats learned phrases verbatim.

### Why Overfitting is Spectacular for Style Synthesis
In **Lume**, however, we are not building a general-purpose model. We do not want this model to write python code, solve math equations, or know about modern politics. We want it to do one thing: write exactly like Alexandre Dumas.

For this specific goal, **overfitting is our greatest asset**:
* **Stylistic Imprinting**: By driving the training loss down to a spectacular near-zero level, the model absorbs the exact transition probabilities of Dumas's vocabulary, sentence lengths, punctuation cadences, and syntactic flow.
* **Memorized Voice**: The model ceases to generate generic LLM-style helper responses. Instead, it naturally maps any prompt context into the authentic voice and terminology of the corpus.
* **Steered Coherence**: Since the model has memorized the corpus style, we can inject search results directly into its prompt. The model then "translates" those factual search results into the exact style of *The Count of Monte Cristo*.

### How We Handle the Downside of Overfitting
While overfitting gives us a perfect Dumas voice, it can occasionally cause the model to generate slightly incoherent or loose phrasing. We mitigate this by using a dual-stage architecture:
1. **The Overfit Style-Synthesizer**: Generates the raw, highly stylized draft response based on the search context.
2. **The Local LLM Editor (Gemma 4 12B)**: Takes the raw draft along with the original search context and polishes it. It fixes any grammatical incoherencies or factual slips, ensuring the final output is both readable and accurate while keeping the authentic Dumas voice intact.
2. **Coherence Cleanup**: Because a small, overfit model can produce output that is stylistically pristine but slightly loose or incoherent, we use a larger, local instruction-tuned model (**Gemma 4 12B**) as a **structural editor** to polish the final response.

---

## 📐 System Architecture & Workflow

The end-to-end inference flow runs entirely locally, combining hybrid search, local model inference, and local LLM editing.

```mermaid
graph TD
    User([User Question]) --> Search[1. Hybrid Search Engine]
    Search -->|Retrieve Context Chunks| Context[Context Compiler]
    Context -->|Context + Question Prompt| OverfitModel[2. Local Overfit Transformer]
    OverfitModel -->|Raw Stylistic Output (Slightly Incoherent)| Editor[3. Local Gemma 4 12B Editor]
    Context -->|Original Context Chunks| Editor
    Editor -->|Polish & Fact-Check| Output([Coherent, Style-Faithful Answer])
```

---

## 🏃‍♀️ Step-by-Step Inference Pipeline

### Step 1: Context Retrieval (Hybrid Search)
When the user submits a question:
- The system runs a hybrid search (lexical BM25 + semantic embeddings) against the persisted `.lume-index`.
- It retrieves the top $K$ matching text chunks (usually 2 chunks of 25 lines each) to serve as the factual grounding.

### Step 2: Stylistic Response Synthesis (Local Overfit Model)
The system formats the retrieved context and question into a prompt and passes it to our trained Transformer:
```text
<|context|>
[Retrieved Chunk 1]
[Retrieved Chunk 2]
<|question|>
[User's Question]
<|answer|>
```
- The local model performs autoregressive inference (`generate.py`), generating a response.
- **Output Character**: The text sounds exactly like *The Count of Monte Cristo*, adopting its tone, vocabulary, and sentence structures, but may contain minor grammatical slips or slight semantic incoherence.

### Step 3: Local LLM Editor & Cleanup (Gemma 4 12B)
To produce a clean final answer, the raw output from the overfit model is sent to the local **Gemma 4 12B** model via Ollama:
- **Prompt to Gemma 4**:
  ```text
  You are a professional editor. You are given a fact-based context, a user question, and a draft answer that is written in the exact style of the book but is slightly incoherent.
  
  Context:
  [Retrieved Chunk 1]
  [Retrieved Chunk 2]
  
  Draft Answer:
  [Raw Output from Overfit Model]
  
  Your task is to edit the Draft Answer to make it completely coherent, grammatically correct, and factually accurate based on the Context. You MUST maintain the literary style, tone, and vocabulary of the Draft Answer.
  ```
- Gemma 4 12B processes the prompt locally (ensuring 100% privacy and no cloud calls) and returns a polished, high-fidelity answer.

---

## 📊 Final Training Results

The local Transformer model trained on the augmented Monte Cristo dataset achieved the following metrics:
* **Model Parameters**: 26.3 Million (`depth=6`, `n_embd=384`)
* **Final Training Loss**: **`0.0310`** (spectacularly low, confirming a tight overfit)
* **Validation Bits Per Byte (bpb)**: **`2.5221`**
* **Total Training Steps**: 380
* **Total Tokens Trained**: 18.7 Million
* **Throughput**: ~61,500 tokens/sec
* **Peak VRAM**: 4,211.3 MB
* **Saved Checkpoint**: `reference_src/checkpoint_monte_cristo.pt`

---

## 🚀 Next Steps (Inference Implementation)

Once training is complete:
1. **Create `autosearch/generate.py`**:
   - Implement the local Transformer loading and text generation interface.
2. **Create `autosearch/query_pipeline.py`**:
   - Tie the hybrid search, local Transformer generation, and Ollama Gemma 4 cleanup API calls into a unified CLI tool:
     `python autosearch/query_pipeline.py "How did Edmond Dantès escape from the Château d’If?"`
